//! Smithay handler/delegate implementations for AppState.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale,
    delegate_layer_shell, delegate_output, delegate_pointer_constraints,
    delegate_pointer_warp, delegate_presentation, delegate_primary_selection,
    delegate_relative_pointer, delegate_seat, delegate_shm, delegate_single_pixel_buffer,
    delegate_viewporter, delegate_virtual_keyboard_manager, delegate_xdg_activation,
    delegate_xdg_decoration, delegate_xdg_shell, delegate_data_control,
    delegate_foreign_toplevel_list,
    desktop::Window,
    input::{
        pointer::CursorImageStatus,
        Seat, SeatHandler, SeatState,
    },
    reexports::wayland_server::{
        protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
        Client,
    },
    utils::{Logical, Point, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        fractional_scale::{with_fractional_scale, FractionalScaleHandler},
        output::OutputHandler,
        seat::WaylandFocus,
        selection::{
            data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler},
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
            wlr_data_control::{DataControlHandler, DataControlState},
            SelectionHandler,
        },
        shell::{
            wlr_layer::{
                Layer as WlrLayer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler,
                WlrLayerShellState,
            },
            xdg::{
                decoration::XdgDecorationHandler,
                PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            },
        },
        shm::{ShmHandler, ShmState},
        foreign_toplevel_list::{ForeignToplevelListHandler, ForeignToplevelListState},
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
        pointer_constraints::{PointerConstraintsHandler},
        pointer_warp::PointerWarpHandler,
    },
};
use smithay::input::pointer::PointerHandle;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::ImportDma;

use crate::state::{AppState, ClientState};

// ---------------------------------------------------------------------------
// Compositor
// ---------------------------------------------------------------------------

impl CompositorHandler for AppState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_client_state
    }
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.space.refresh();

        // Call on_commit() for any already-mapped window whose surface was committed.
        // This updates the window's cached geometry/state so render_elements_for_output
        // can produce elements for it. Must happen before the pending_windows check.
        if let Some(window) = self.space.elements()
            .find(|w| w.wl_surface().as_deref() == Some(surface))
            .cloned()
        {
            window.on_commit();
        }

        // Map newly-configured windows into the space on their first commit.
        if let Some(window) = self.pending_windows.iter().find(|w| {
            w.wl_surface().map(|s| &*s == surface).unwrap_or(false)
        }).cloned() {
            tracing::info!("Mapping window at (0,0)");
            self.space.map_element(window.clone(), (0, 0), true);
            self.pending_windows.retain(|w| w != &window);
            self.is_capturing = true;
        }
    }
}
delegate_compositor!(AppState);

// ---------------------------------------------------------------------------
// SHM
// ---------------------------------------------------------------------------

impl BufferHandler for AppState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}
impl ShmHandler for AppState {
    fn shm_state(&self) -> &ShmState { &self.shm_state }
}
delegate_shm!(AppState);

// ---------------------------------------------------------------------------
// DMABUF
// ---------------------------------------------------------------------------

impl DmabufHandler for AppState {
    fn dmabuf_state(&mut self) -> &mut DmabufState { &mut self.dmabuf_state }
    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        if self.gles_renderer.as_mut()
            .map(|r| r.import_dmabuf(&dmabuf, None).is_ok())
            .unwrap_or(false)
        {
            let _ = notifier.successful::<AppState>();
        } else {
            notifier.failed();
        }
    }
}
delegate_dmabuf!(AppState);

// ---------------------------------------------------------------------------
// Seat — use WlSurface directly as focus target (no X11, no decoration SSD)
// ---------------------------------------------------------------------------

impl SeatHandler for AppState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<AppState> { &mut self.seat_state }
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
        use smithay::input::pointer::CursorImageSurfaceData;
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shm::with_buffer_contents;
        use bytes::Bytes;
        use crate::types::CursorEvent;

        let event = match &image {
            // Named cursor (including the default arrow) → let the browser show its own cursor.
            CursorImageStatus::Named(_) => CursorEvent::Default,
            CursorImageStatus::Hidden => CursorEvent::Hidden,
            CursorImageStatus::Surface(surface) => {
                // Read hotspot from CursorImageSurfaceData (type alias = Mutex<CursorImageAttributes>).
                let (hotspot_x, hotspot_y) = with_states(surface, |states| {
                    states.data_map
                        .get::<CursorImageSurfaceData>()
                        .and_then(|m| m.lock().ok())
                        .map(|attrs| (attrs.hotspot.x, attrs.hotspot.y))
                        .unwrap_or((0, 0))
                });

                // Read RGBA pixels from the committed SHM buffer via the renderer surface state.
                let pixel_result = with_states(surface, |states| {
                    let state = states.data_map.get::<RendererSurfaceStateUserData>()?;
                    let locked = state.lock().ok()?;
                    let wl_buffer = locked.buffer()?.clone();
                    drop(locked);

                    // SAFETY: ptr is valid for `len` bytes for the duration of this closure.
                    let result = with_buffer_contents(&wl_buffer, |ptr, _len, spec| {
                        let width  = spec.width as u32;
                        let height = spec.height as u32;
                        let stride = spec.stride as usize;
                        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                        for row in 0..height as usize {
                            for col in 0..width as usize {
                                // WL_SHM_FORMAT_ARGB8888 on LE: stored as [B, G, R, A]
                                let px = unsafe { ptr.add(row * stride + col * 4) };
                                unsafe {
                                    rgba.push(*px.add(2)); // R
                                    rgba.push(*px.add(1)); // G
                                    rgba.push(*px       ); // B
                                    rgba.push(*px.add(3)); // A
                                }
                            }
                        }
                        (width, height, rgba)
                    });
                    result.ok()
                });

                match pixel_result {
                    Some((width, height, rgba)) => CursorEvent::Image {
                        width,
                        height,
                        hotspot_x,
                        hotspot_y,
                        rgba: Bytes::from(rgba),
                    },
                    None => CursorEvent::Default,
                }
            }
        };

        let _ = self.cursor_tx.send(event);
        self.current_cursor_icon = Some(image);
    }
}
delegate_seat!(AppState);

// ---------------------------------------------------------------------------
// XDG Shell
// ---------------------------------------------------------------------------

impl XdgShellHandler for AppState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState { &mut self.shell_state }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        tracing::info!("New XDG toplevel window");
        // XDG shell requires an initial configure before the client will
        // commit its first buffer. Set the desired size and states, then
        // send the configure so the client knows it can start rendering.
        surface.with_pending_state(|state| {
            use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
            state.size = Some((self.width, self.height).into());
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        self.pending_windows.push(Window::new_wayland_window(surface));
    }
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}
    fn grab(&mut self, _surface: PopupSurface, _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat, _serial: Serial) {}
    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|s| { s.geometry = positioner.get_geometry(); s.positioner = positioner; });
        surface.send_repositioned(token);
    }
}
delegate_xdg_shell!(AppState);

// ---------------------------------------------------------------------------
// WLR Layer shell
// ---------------------------------------------------------------------------

impl WlrLayerShellHandler for AppState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState { &mut self.layer_shell_state }
    fn new_layer_surface(&mut self, _surface: WlrLayerSurface, _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>, _layer: WlrLayer, _namespace: String) {}
    fn layer_destroyed(&mut self, _surface: WlrLayerSurface) {}
}
delegate_layer_shell!(AppState);

// ---------------------------------------------------------------------------
// Output / Selection / Clipboard
// ---------------------------------------------------------------------------

impl OutputHandler for AppState {}
delegate_output!(AppState);

impl SelectionHandler for AppState {
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        ty: smithay::wayland::selection::SelectionTarget,
        source: Option<smithay::wayland::selection::SelectionSource>,
        _seat: smithay::input::Seat<Self>,
    ) {
        use smithay::wayland::selection::SelectionTarget;
        use crate::types::ClipboardEvent;

        if ty != SelectionTarget::Clipboard {
            return;
        }

        match source {
            None => {
                tracing::debug!("Clipboard cleared");
                self.pending_clipboard_mime = None;
                let _ = self.clipboard_tx.send(ClipboardEvent::Cleared);
            }
            Some(ref src) => {
                let mime_types = src.mime_types();
                tracing::debug!("new_selection: mime_types={:?}", mime_types);
                let preferred = [
                    "text/plain;charset=utf-8",
                    "text/plain",
                    "UTF8_STRING",
                    "STRING",
                    "TEXT",
                ];
                self.pending_clipboard_mime = preferred
                    .iter()
                    .find(|m| mime_types.contains(&m.to_string()))
                    .map(|s| s.to_string());
                tracing::debug!("pending_clipboard_mime={:?}", self.pending_clipboard_mime);
            }
        }
    }

    fn send_selection(
        &mut self,
        ty: smithay::wayland::selection::SelectionTarget,
        mime_type: String,
        fd: std::os::unix::io::OwnedFd,
        _seat: smithay::input::Seat<Self>,
        _user_data: &(),
    ) {
        use smithay::wayland::selection::SelectionTarget;

        if ty != SelectionTarget::Clipboard {
            return;
        }

        let Some(ref text) = self.clipboard_contents else { return };

        // Determine the encoding for the requested MIME type.
        let data = if mime_type == "text/plain;charset=utf-8"
            || mime_type == "text/plain"
            || mime_type == "UTF8_STRING"
            || mime_type == "STRING"
            || mime_type == "TEXT"
        {
            text.as_bytes().to_vec()
        } else {
            return;
        };

        // Write in a background thread so we don't block the compositor event loop.
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = std::fs::File::from(fd).write_all(&data);
        });
    }
}

impl DataDeviceHandler for AppState {
    fn data_device_state(&mut self) -> &mut DataDeviceState { &mut self.data_device_state }
}
impl WaylandDndGrabHandler for AppState {}
delegate_data_device!(AppState);

impl PrimarySelectionHandler for AppState {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState { &mut self.primary_selection_state }
}
delegate_primary_selection!(AppState);

impl DataControlHandler for AppState {
    fn data_control_state(&mut self) -> &mut DataControlState { &mut self.data_control_state }
}
delegate_data_control!(AppState);

// ---------------------------------------------------------------------------
// Virtual keyboard
// ---------------------------------------------------------------------------
delegate_virtual_keyboard_manager!(AppState);

// ---------------------------------------------------------------------------
// Fractional scale / viewporter / presentation / single-pixel-buffer
// ---------------------------------------------------------------------------

impl FractionalScaleHandler for AppState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        smithay::wayland::compositor::with_states(&surface, |states| {
            with_fractional_scale(states, |s| s.set_preferred_scale(1.0));
        });
    }
}
delegate_fractional_scale!(AppState);
delegate_viewporter!(AppState);
delegate_presentation!(AppState);
delegate_single_pixel_buffer!(AppState);

// ---------------------------------------------------------------------------
// Foreign toplevel list / XDG decoration / XDG activation
// ---------------------------------------------------------------------------

impl ForeignToplevelListHandler for AppState {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState { &mut self.foreign_toplevel_list }
}
delegate_foreign_toplevel_list!(AppState);

impl XdgDecorationHandler for AppState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        toplevel.with_pending_state(|s| { s.decoration_mode = Some(Mode::ServerSide); });
    }
    fn request_mode(&mut self, _t: ToplevelSurface, _m: smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode) {}
    fn unset_mode(&mut self, _t: ToplevelSurface) {}
}
delegate_xdg_decoration!(AppState);

impl XdgActivationHandler for AppState {
    fn activation_state(&mut self) -> &mut XdgActivationState { &mut self.xdg_activation_state }
    fn token_created(&mut self, _token: XdgActivationToken, _data: XdgActivationTokenData) -> bool { true }
    fn request_activation(&mut self, _token: XdgActivationToken, _data: XdgActivationTokenData, _surface: WlSurface) {}
}
delegate_xdg_activation!(AppState);

// ---------------------------------------------------------------------------
// Pointer constraints / warp / relative pointer
// ---------------------------------------------------------------------------

impl PointerConstraintsHandler for AppState {
    fn new_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {}
    fn cursor_position_hint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>, _location: Point<f64, Logical>) {}
}
delegate_pointer_constraints!(AppState);

impl PointerWarpHandler for AppState {}
delegate_pointer_warp!(AppState);
delegate_relative_pointer!(AppState);
