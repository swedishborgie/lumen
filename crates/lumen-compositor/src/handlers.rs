//! Smithay handler/delegate implementations for AppState.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_dmabuf,
    delegate_fractional_scale, delegate_layer_shell, delegate_output,
    delegate_pointer_constraints, delegate_pointer_warp, delegate_presentation,
    delegate_primary_selection, delegate_relative_pointer, delegate_seat, delegate_shm,
    delegate_single_pixel_buffer, delegate_viewporter, delegate_virtual_keyboard_manager,
    delegate_xdg_activation, delegate_xdg_decoration, delegate_xdg_shell, delegate_data_control,
    delegate_foreign_toplevel_list,
    desktop::Window,
    input::{
        pointer::{CursorIcon, CursorImageStatus},
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
        pointer_constraints::{PointerConstraintsHandler, with_pointer_constraint},
        pointer_warp::PointerWarpHandler,
    },
};
use smithay::input::pointer::PointerHandle;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::{ImportDma, ExportMem};

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
        use smithay::input::pointer::CursorImageSurfaceData;
        use smithay::wayland::compositor::with_states;
        use bytes::Bytes;
        use crate::types::CursorEvent;

        let event = match &image {
            // Named cursor: convert to the corresponding CSS cursor name.
            CursorImageStatus::Named(icon) => {
                let css = cursor_icon_to_css(icon);
                tracing::debug!("cursor_image: Named({icon:?}) -> css={css:?}");
                CursorEvent::Named(css.to_string())
            }
            CursorImageStatus::Hidden => {
                tracing::debug!("cursor_image: Hidden");
                CursorEvent::Hidden
            }
            CursorImageStatus::Surface(surface) => {
                // Read hotspot from CursorImageSurfaceData (type alias = Mutex<CursorImageAttributes>).
                let (hotspot_x, hotspot_y) = with_states(surface, |states| {
                    states.data_map
                        .get::<CursorImageSurfaceData>()
                        .and_then(|m| m.lock().ok())
                        .map(|attrs| (attrs.hotspot.x, attrs.hotspot.y))
                        .unwrap_or((0, 0))
                });

                // Try CPU paths first (SHM + linear DMA-BUF); fall back to GPU
                // readback for tiled DMA-BUF buffers (e.g. Intel X-tiled on kwin).
                let pixel_result = read_cursor_surface_pixels(surface)
                    .or_else(|| read_cursor_surface_pixels_gpu(surface, self.gles_renderer.as_mut()?));

                match pixel_result {
                    Some((width, height, rgba)) => {
                        tracing::debug!("cursor_image: Surface {width}x{height} hotspot=({hotspot_x},{hotspot_y})");
                        CursorEvent::Image {
                            width,
                            height,
                            hotspot_x,
                            hotspot_y,
                            rgba: Bytes::from(rgba),
                        }
                    }
                    None => {
                        tracing::debug!("cursor_image: all read paths failed, falling back to Default");
                        CursorEvent::Default
                    }
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
            tracing::trace!("new_selection: ignoring non-clipboard target {:?}", ty);
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

        tracing::debug!("send_selection: ty={:?} mime_type={:?}", ty, mime_type);

        if ty != SelectionTarget::Clipboard {
            tracing::trace!("send_selection: ignoring non-clipboard target {:?}", ty);
            return;
        }

        let Some(ref text) = self.clipboard_contents else {
            tracing::debug!("send_selection: no clipboard contents, ignoring request");
            return;
        };

        // Determine the encoding for the requested MIME type.
        let data = if mime_type == "text/plain;charset=utf-8"
            || mime_type == "text/plain"
            || mime_type == "UTF8_STRING"
            || mime_type == "STRING"
            || mime_type == "TEXT"
        {
            text.as_bytes().to_vec()
        } else {
            tracing::debug!("send_selection: unsupported mime_type={:?}, ignoring", mime_type);
            return;
        };

        tracing::debug!("send_selection: serving {} bytes for mime_type={:?}", data.len(), mime_type);

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
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Activate the constraint immediately so that the client receives the
        // `locked` / `confined` event and can proceed with its input setup.
        // Fullscreen games (e.g. SDL-based Steam games) require this to start
        // consuming relative pointer motion.
        with_pointer_constraint(surface, pointer, |constraint| {
            if let Some(c) = constraint {
                c.activate();
            }
        });
    }
    fn cursor_position_hint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>, _location: Point<f64, Logical>) {}
}
delegate_pointer_constraints!(AppState);

impl PointerWarpHandler for AppState {}
delegate_pointer_warp!(AppState);
delegate_relative_pointer!(AppState);

// ---------------------------------------------------------------------------
// wp_cursor_shape_manager_v1
// ---------------------------------------------------------------------------

impl smithay::wayland::tablet_manager::TabletSeatHandler for AppState {
    fn tablet_tool_image(
        &mut self,
        _tool: &smithay::backend::input::TabletToolDescriptor,
        _image: CursorImageStatus,
    ) {
        // Tablet cursor shape changes are not forwarded; tablet support is not enabled.
    }
}
delegate_cursor_shape!(AppState);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a Smithay [`CursorIcon`] to the equivalent CSS `cursor` property value.
fn cursor_icon_to_css(icon: &CursorIcon) -> &'static str {
    match icon {
        CursorIcon::Default      => "default",
        CursorIcon::ContextMenu  => "context-menu",
        CursorIcon::Help         => "help",
        CursorIcon::Pointer      => "pointer",
        CursorIcon::Progress     => "progress",
        CursorIcon::Wait         => "wait",
        CursorIcon::Cell         => "cell",
        CursorIcon::Crosshair    => "crosshair",
        CursorIcon::Text         => "text",
        CursorIcon::VerticalText => "vertical-text",
        CursorIcon::Alias        => "alias",
        CursorIcon::Copy         => "copy",
        CursorIcon::Move         => "move",
        CursorIcon::NoDrop       => "no-drop",
        CursorIcon::NotAllowed   => "not-allowed",
        CursorIcon::Grab         => "grab",
        CursorIcon::Grabbing     => "grabbing",
        CursorIcon::EResize      => "e-resize",
        CursorIcon::NResize      => "n-resize",
        CursorIcon::NeResize     => "ne-resize",
        CursorIcon::NwResize     => "nw-resize",
        CursorIcon::SResize      => "s-resize",
        CursorIcon::SeResize     => "se-resize",
        CursorIcon::SwResize     => "sw-resize",
        CursorIcon::WResize      => "w-resize",
        CursorIcon::EwResize     => "ew-resize",
        CursorIcon::NsResize     => "ns-resize",
        CursorIcon::NeswResize   => "nesw-resize",
        CursorIcon::NwseResize   => "nwse-resize",
        CursorIcon::ColResize    => "col-resize",
        CursorIcon::RowResize    => "row-resize",
        CursorIcon::AllScroll    => "all-scroll",
        CursorIcon::ZoomIn       => "zoom-in",
        CursorIcon::ZoomOut      => "zoom-out",
        // DndAsk and AllResize have no direct CSS equivalent.
        _ => "default",
    }
}

/// Read RGBA pixel data from a committed cursor surface.
///
/// Tries SHM first, then falls back to DMA-BUF mmap for GPU-allocated cursor buffers.
/// Returns `None` if the buffer cannot be read (e.g. tiled DMA-BUF, unsupported format).
fn read_cursor_surface_pixels(surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> Option<(u32, u32, Vec<u8>)> {
    use smithay::backend::allocator::dmabuf::{DmabufMappingMode, DmabufSyncFlags};
    use smithay::backend::allocator::{Buffer as AllocBuffer, Fourcc};
    use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
    use smithay::wayland::compositor::with_states;
    use smithay::wayland::dmabuf::get_dmabuf;
    use smithay::wayland::shm::with_buffer_contents;

    with_states(surface, |states| {
        let state = states.data_map.get::<RendererSurfaceStateUserData>()
            .or_else(|| { tracing::debug!("cursor read: no RendererSurfaceStateUserData"); None })?;
        let locked = state.lock().ok()
            .or_else(|| { tracing::debug!("cursor read: lock failed"); None })?;
        let buf = locked.buffer().cloned()
            .or_else(|| { tracing::debug!("cursor read: buffer() is None (surface not yet committed with a buffer)"); None })?;
        drop(locked);

        // ── Path 1: SHM buffer ───────────────────────────────────────────────
        let shm_result = with_buffer_contents(&*buf, |ptr, _len, spec| {
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
        if let Ok(result) = shm_result {
            tracing::debug!("cursor read: SHM path ok");
            return Some(result);
        }

        // ── Path 2: DMA-BUF buffer (mmap, linear only) ───────────────────────
        let dmabuf = match get_dmabuf(&*buf) {
            Ok(d) => d,
            Err(_) => {
                tracing::debug!("cursor read: not SHM and not DMA-BUF — unknown buffer type");
                return None;
            }
        };

        if dmabuf.has_modifier() {
            tracing::debug!(
                "cursor read: DMA-BUF has tiling modifier ({:?}), trying GPU readback path",
                dmabuf.format().modifier
            );
            return None;
        }

        let width  = dmabuf.size().w as u32;
        let height = dmabuf.size().h as u32;
        let stride = dmabuf.strides().next().unwrap_or(width * 4) as usize;
        let fourcc = dmabuf.format().code;

        tracing::debug!("cursor read: DMA-BUF path {width}x{height} stride={stride} fourcc={fourcc:?}");

        // Sync CPU-side before reading.
        let _ = dmabuf.sync_plane(0, DmabufSyncFlags::READ | DmabufSyncFlags::START);
        let mapping = dmabuf.map_plane(0, DmabufMappingMode::READ).ok()
            .or_else(|| { tracing::debug!("cursor read: DMA-BUF mmap failed"); None })?;

        let ptr = mapping.ptr() as *const u8;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        let ok = (|| -> Option<()> {
            for row in 0..height as usize {
                for col in 0..width as usize {
                    let offset = row * stride + col * 4;
                    if offset + 3 >= mapping.length() { return None; }
                    let px = unsafe { ptr.add(offset) };
                    match fourcc {
                        Fourcc::Argb8888 => unsafe {
                            // LE layout: [B, G, R, A]
                            rgba.push(*px.add(2)); // R
                            rgba.push(*px.add(1)); // G
                            rgba.push(*px       ); // B
                            rgba.push(*px.add(3)); // A
                        },
                        Fourcc::Xrgb8888 => unsafe {
                            // LE layout: [B, G, R, X] — treat X as fully opaque
                            rgba.push(*px.add(2)); // R
                            rgba.push(*px.add(1)); // G
                            rgba.push(*px       ); // B
                            rgba.push(255);        // A
                        },
                        other => {
                            tracing::debug!("cursor read: unsupported DMA-BUF fourcc {other:?}");
                            return None;
                        }
                    }
                }
            }
            Some(())
        })();

        // Sync CPU-side after reading.
        let _ = dmabuf.sync_plane(0, DmabufSyncFlags::READ | DmabufSyncFlags::END);
        drop(mapping);

        ok.map(|_| (width, height, rgba))
    })
}

/// GPU readback path for tiled DMA-BUF cursor surfaces (e.g. Intel X-tiled on KWin).
///
/// Imports the DMA-BUF as a GL texture, copies it to a PBO via `ExportMem::copy_texture`,
/// then maps the PBO to get RGBA bytes. Returns `None` on any GL failure.
fn read_cursor_surface_pixels_gpu(
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
) -> Option<(u32, u32, Vec<u8>)> {
    use smithay::backend::allocator::{Buffer as AllocBuffer, Fourcc};
    use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
    use smithay::utils::Rectangle;
    use smithay::wayland::compositor::with_states;
    use smithay::wayland::dmabuf::get_dmabuf;

    // Clone the DMA-BUF out of with_states so we can call mutable renderer methods after.
    let dmabuf = with_states(surface, |states| {
        let state = states.data_map.get::<RendererSurfaceStateUserData>()?;
        let locked = state.lock().ok()?;
        let buf = locked.buffer().cloned()?;
        drop(locked);
        get_dmabuf(&*buf).ok().cloned()
    })?;

    let width  = dmabuf.size().w as u32;
    let height = dmabuf.size().h as u32;
    tracing::debug!("cursor GPU readback: importing {width}x{height} DMA-BUF modifier={:?}", dmabuf.format().modifier);

    let texture = renderer.import_dmabuf(&dmabuf, None)
        .map_err(|e| tracing::debug!("cursor GPU readback: import_dmabuf failed: {e:?}"))
        .ok()?;

    let region = Rectangle::new((0, 0).into(), (width as i32, height as i32).into());
    // Abgr8888 = GL_RGBA + GL_UNSIGNED_BYTE → bytes stored as [R, G, B, A] — RGBA order.
    let mapping = renderer.copy_texture(&texture, region, Fourcc::Abgr8888)
        .map_err(|e| tracing::debug!("cursor GPU readback: copy_texture failed: {e:?}"))
        .ok()?;

    let data = renderer.map_texture(&mapping)
        .map_err(|e| tracing::debug!("cursor GPU readback: map_texture failed: {e:?}"))
        .ok()?;

    let rgba = data.to_vec();
    tracing::debug!("cursor GPU readback: ok — {} bytes for {width}x{height}", rgba.len());
    Some((width, height, rgba))
}
