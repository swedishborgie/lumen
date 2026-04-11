use std::fs::File;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gbm::{BufferObject, BufferObjectFlags, Device as RawGbmDevice, Format as GbmFormat};
use smithay::{
    backend::{
        allocator::{dmabuf::Dmabuf, gbm::GbmDevice},
        drm::DrmNode,
        egl::{EGLContext, EGLDisplay},
        renderer::{
            damage::OutputDamageTracker,
            gles::GlesRenderer,
            pixman::PixmanRenderer,
            Bind,
        },
    },
    desktop::{PopupManager, Space},
    input::{keyboard::XkbConfig, SeatState},
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, Mode, PostAction,
        },
        wayland_server::Display,
    },
    utils::{Clock, Transform},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufFeedbackBuilder, DmabufState},
        foreign_toplevel_list::ForeignToplevelListState,
        fractional_scale::FractionalScaleManagerState,
        output::OutputManagerState,
        cursor_shape::CursorShapeManagerState,
        pointer_constraints::PointerConstraintsState,
        pointer_warp::PointerWarpManager,
        presentation::PresentationState,
        selection::{
            data_device::DataDeviceState,
            primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        shell::{wlr_layer::WlrLayerShellState, xdg::XdgShellState, xdg::decoration::XdgDecorationState},
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        virtual_keyboard::VirtualKeyboardManagerState,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
        relative_pointer::RelativePointerManagerState,
    },
};
use tokio::sync::broadcast;

use crate::input::InputEvent;
use crate::render::render_and_capture;
use crate::state::{AppState, ClientState, CompositorCommand};
use crate::types::{CapturedFrame, CompositorConfig, CursorEvent, ClipboardEvent};

/// A cheaply-cloneable handle for sending input events into the compositor.
///
/// Wraps the calloop command channel so that external callers don't need to
/// know about `CompositorCommand`.
#[derive(Clone)]
pub struct InputSender(smithay::reexports::calloop::channel::Sender<CompositorCommand>);

impl InputSender {
    pub fn send(&self, ev: InputEvent) {
        let _ = self.0.send(CompositorCommand::Input(ev));
    }

    /// Request a live resize of the compositor output.
    /// Width and height must be positive even numbers ≤ 4096.
    pub fn resize(&self, width: u32, height: u32) {
        let _ = self.0.send(CompositorCommand::Resize(width, height));
    }

    /// Set the compositor clipboard to the given text.
    pub fn clipboard_write(&self, text: String) {
        let _ = self.0.send(CompositorCommand::ClipboardWrite(text));
    }

    /// Signal the compositor event loop to stop.
    pub fn stop(&self) {
        let _ = self.0.send(CompositorCommand::Stop);
    }
}

/// Smithay-based Wayland compositor.
pub struct Compositor {
    config: CompositorConfig,
    frame_tx: broadcast::Sender<CapturedFrame>,
    cursor_tx: broadcast::Sender<CursorEvent>,
    clipboard_tx: broadcast::Sender<ClipboardEvent>,
    cmd_tx: smithay::reexports::calloop::channel::Sender<CompositorCommand>,
    cmd_rx: Option<smithay::reexports::calloop::channel::Channel<CompositorCommand>>,
}

impl Compositor {
    pub fn new(config: CompositorConfig) -> Result<Self> {
        let (frame_tx, _) = broadcast::channel(8);
        let (cursor_tx, _) = broadcast::channel(16);
        let (clipboard_tx, _) = broadcast::channel(16);
        let (cmd_tx, cmd_rx) = smithay::reexports::calloop::channel::channel();
        Ok(Self { config, frame_tx, cursor_tx, clipboard_tx, cmd_tx, cmd_rx: Some(cmd_rx) })
    }

    pub fn input_sender(&self) -> InputSender { InputSender(self.cmd_tx.clone()) }
    pub fn frame_receiver(&self) -> broadcast::Receiver<CapturedFrame> { self.frame_tx.subscribe() }
    pub fn cursor_receiver(&self) -> broadcast::Receiver<CursorEvent> { self.cursor_tx.subscribe() }
    pub fn clipboard_receiver(&self) -> broadcast::Receiver<ClipboardEvent> { self.clipboard_tx.subscribe() }
    pub fn stop(&self) { let _ = self.cmd_tx.send(CompositorCommand::Stop); }

    /// Blocking compositor event loop. Call from a dedicated `std::thread`.
    pub fn run(&mut self) -> Result<()> {
        tracing::info!("Compositor starting ({}x{} @ {}fps)",
            self.config.width, self.config.height, self.config.target_fps);

        let cmd_rx = self.cmd_rx.take().context("run() called twice")?;
        let width = self.config.width as i32;
        let height = self.config.height as i32;
        let target_fps = self.config.target_fps;
        let dri_node_path = self.config.render_node.clone();
        let frame_tx = self.frame_tx.clone();
        let cursor_tx = self.cursor_tx.clone();
        let clipboard_tx = self.clipboard_tx.clone();
        let peer_count = self.config.peer_count.clone();
        let peer_count_loop = peer_count.clone();

        let mut event_loop = EventLoop::<AppState>::try_new()
            .context("Failed to create calloop EventLoop")?;
        let display: Display<AppState> = Display::new()
            .context("Failed to create Wayland display")?;
        let dh = display.handle();

        // -----------------------------------------------------------------------
        // GPU or CPU renderer
        // -----------------------------------------------------------------------
        let use_gpu = dri_node_path.is_some();
        let mut gles_renderer: Option<GlesRenderer> = None;
        let mut pixman_renderer: Option<PixmanRenderer> = None;
        let mut offscreen_buffers: Vec<(BufferObject<()>, Dmabuf, u64)> = Vec::new();
        let mut gbm_device_raw = None;
        let mut dmabuf_state = DmabufState::new();
        let mut dmabuf_global = None;

        if let Some(ref node_path) = dri_node_path {
            tracing::info!("GPU renderer: {}", node_path.display());
            let file = File::options().read(true).write(true).open(node_path)
                .with_context(|| format!("Failed to open DRI node {}", node_path.display()))?;
            let file_for_gbm = file.try_clone().context("Failed to clone DRI fd")?;
            let gbm_alloc = RawGbmDevice::new(file_for_gbm)
                .context("Failed to create raw GBM device")?;
            let gbm = GbmDevice::new(file).context("Failed to create GBM device")?;
            let egl = unsafe { EGLDisplay::new(gbm) }.context("Failed to create EGL display")?;
            let ctx = EGLContext::new(&egl).context("Failed to create EGL context")?;
            let renderer = unsafe { GlesRenderer::new(ctx) }.context("Failed to init GlesRenderer")?;

            let formats: Vec<_> = Bind::<Dmabuf>::supported_formats(&renderer)
                .context("Failed to query DMA-BUF formats")?
                .into_iter().collect();
            let drm_node = DrmNode::from_path(node_path).context("Failed to create DRM node")?;
            if let Ok(feedback) = DmabufFeedbackBuilder::new(drm_node.dev_id(), formats.clone()).build() {
                dmabuf_global = Some(dmabuf_state.create_global_with_default_feedback::<AppState>(&dh, &feedback));
            } else {
                dmabuf_global = Some(dmabuf_state.create_global::<AppState>(&dh, formats));
            }
            offscreen_buffers = allocate_offscreen_ring(
                &gbm_alloc, width.cast_unsigned(), height.cast_unsigned(),
            ).context("Failed to allocate offscreen ring buffers")?;
            gbm_device_raw = Some(gbm_alloc);
            gles_renderer = Some(renderer);
        } else {
            tracing::info!("CPU renderer (Pixman)");
            pixman_renderer = Some(PixmanRenderer::new().context("Failed to init PixmanRenderer")?);
        }

        // -----------------------------------------------------------------------
        // Wayland protocol state
        // -----------------------------------------------------------------------
        let compositor_state = CompositorState::new::<AppState>(&dh);
        let fractional_scale_state = FractionalScaleManagerState::new::<AppState>(&dh);
        let shm_state = ShmState::new::<AppState>(&dh, vec![]);
        let output_state = OutputManagerState::new_with_xdg_output::<AppState>(&dh);
        let mut seat_state = SeatState::new();
        let shell_state = XdgShellState::new::<AppState>(&dh);
        let space = Space::default();
        let layer_shell_state = WlrLayerShellState::new::<AppState>(&dh);
        let data_device_state = DataDeviceState::new::<AppState>(&dh);
        let data_control_state = DataControlState::new::<AppState, _>(&dh, None, |_| true);
        let virtual_keyboard_state = VirtualKeyboardManagerState::new::<AppState, _>(&dh, |_| true);
        let pointer_warp_state = PointerWarpManager::new::<AppState>(&dh);
        let relative_pointer_state = RelativePointerManagerState::new::<AppState>(&dh);
        let pointer_constraints_state = PointerConstraintsState::new::<AppState>(&dh);
        let cursor_shape_state = CursorShapeManagerState::new::<AppState>(&dh);
        let foreign_toplevel_list = ForeignToplevelListState::new::<AppState>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<AppState>(&dh);
        let single_pixel_buffer = SinglePixelBufferState::new::<AppState>(&dh);
        let viewporter_state = ViewporterState::new::<AppState>(&dh);
        let presentation_state = PresentationState::new::<AppState>(&dh, 1);
        let xdg_activation_state = XdgActivationState::new::<AppState>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<AppState>(&dh);
        let popups = PopupManager::default();

        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(XkbConfig::default(), 200, 25).context("Failed to add keyboard")?;
        seat.add_pointer();

        let mut state = AppState {
            compositor_state, fractional_scale_state, viewporter_state, presentation_state,
            shm_state, single_pixel_buffer, dmabuf_state, dmabuf_global, output_state,
            seat_state, shell_state, layer_shell_state, space, data_device_state,
            data_control_state, dh: dh.clone(), seat, virtual_keyboard_state,
            pointer_warp_state, relative_pointer_state, pointer_constraints_state,
            cursor_shape_state,
            outputs: Vec::new(), pending_windows: Vec::new(), foreign_toplevel_list,
            xdg_decoration_state, xdg_activation_state, primary_selection_state, popups,
            frame_buffer: vec![0u8; usize::try_from(width * height * 4).expect("frame buffer size fits usize")],
            gles_renderer, pixman_renderer, gbm_device: gbm_device_raw, offscreen_buffers,
            offscreen_index: 0,
            damage_tracker: None,
            is_capturing: true, width, height, target_fps, frame_tx, cursor_tx, clipboard_tx,
            frame_counter: 0, clock: Clock::new(), current_cursor_icon: None,
            clipboard_contents: None, pending_clipboard_mime: None,
            clipboard_sent_text: Arc::new(std::sync::Mutex::new(None)),
            bridge_write_tx: None,
            last_log_time: Instant::now(), encoded_frame_count: 0, start_time: Instant::now(),
            use_gpu,
            cursor_pos: smithay::utils::Point::from((0.0, 0.0)),
        };

        // -----------------------------------------------------------------------
        // Virtual output
        // -----------------------------------------------------------------------
        let output = Output::new(
            "LUMEN-1".into(),
            PhysicalProperties {
                size: (width, height).into(),
                subpixel: Subpixel::Unknown,
                make: "Lumen".into(),
                model: "Virtual".into(),
                serial_number: "001".into(),
            },
        );
        #[allow(clippy::cast_possible_truncation)]
        let refresh_mhz = (target_fps * 1000.0).round() as i32;
        let mode = OutputMode { size: (width, height).into(), refresh: refresh_mhz };
        output.change_current_state(Some(mode), Some(Transform::Normal), Some(OutputScale::Integer(1)), Some((0, 0).into()));
        output.set_preferred(mode);
        state.space.map_output(&output, (0, 0));
        state.outputs.push(output.clone());
        let _output_global = output.create_global::<AppState>(&dh);
        state.damage_tracker = Some(OutputDamageTracker::from_output(&output));

        // -----------------------------------------------------------------------
        // Wayland display source (calloop watches the display fd)
        // -----------------------------------------------------------------------
        event_loop.handle()
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: we don't drop the display inside this callback.
                    unsafe { display.get_mut().dispatch_clients(state).unwrap(); }
                    Ok(PostAction::Continue)
                },
            )
            .expect("Failed to insert Wayland display source");

        // -----------------------------------------------------------------------
        // Wayland socket source
        // -----------------------------------------------------------------------
        let socket_source = ListeningSocketSource::new_auto()
            .context("Failed to create Wayland socket")?;
        let socket_name = socket_source.socket_name().to_string_lossy().into_owned();
        tracing::info!("Wayland socket: {}", socket_name);
        // Notify any listener (e.g. a --launch task) that the socket is ready.
        if let Some(tx) = self.config.socket_name_tx.take() {
            let _ = tx.send(socket_name.clone());
        }
        // SAFETY: setenv is not thread-safe in general, but we do it once before
        // any other threads can observe WAYLAND_DISPLAY.
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
        event_loop.handle()
            .insert_source(socket_source, |client_stream, _, state| {
                tracing::info!("New Wayland client connected");
                if let Err(e) = state.dh.insert_client(client_stream, Arc::new(ClientState::default())) {
                    tracing::error!("Failed to add Wayland client: {:?}", e);
                }
            })
            .expect("Failed to insert Wayland socket source");

        // -----------------------------------------------------------------------
        // Command channel (stop signal)
        // -----------------------------------------------------------------------
        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag2 = stop_flag.clone();
        event_loop.handle()
            .insert_source(cmd_rx, move |event, _, state| {
                use smithay::reexports::calloop::channel::Event as CalloopEvent;
                match event {
                    CalloopEvent::Msg(CompositorCommand::Stop) => {
                        stop_flag2.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    CalloopEvent::Msg(CompositorCommand::Input(ev)) => {
                        if let InputEvent::SetTargetFps(fps) = ev {
                            crate::compositor::apply_set_target_fps(state, fps);
                        } else {
                            crate::input::inject_input(state, ev);
                        }
                    }
                    CalloopEvent::Msg(CompositorCommand::Resize(w, h)) => {
                        crate::compositor::apply_resize(state, w, h);
                    }
                    CalloopEvent::Msg(CompositorCommand::ClipboardWrite(text)) => {
                        crate::compositor::apply_clipboard_write(state, text);
                    }
                    CalloopEvent::Closed => {}
                }
            })
            .expect("Failed to insert command channel source");

        // -----------------------------------------------------------------------
        // Clipboard bridge (optional, requires --inner-display)
        // -----------------------------------------------------------------------
        if let Some(inner_display) = self.config.inner_display.clone() {
            let (bridge_write_tx, bridge_write_rx) = std::sync::mpsc::sync_channel::<String>(8);
            state.bridge_write_tx = Some(bridge_write_tx);
            let clipboard_tx = self.clipboard_tx.clone();
            let clipboard_sent_text = Arc::clone(&state.clipboard_sent_text);
            // The compositor runs on a bare std::thread (no Tokio runtime), so we
            // spawn a plain OS thread rather than tokio::task::spawn_blocking.
            std::thread::Builder::new()
                .name("clipboard-bridge".into())
                .spawn(move || {
                    crate::clipboard_bridge::run(
                        inner_display,
                        clipboard_tx,
                        clipboard_sent_text,
                        bridge_write_rx,
                    );
                })
                .expect("Failed to spawn clipboard bridge thread");
        }

        // -----------------------------------------------------------------------
        // Frame timer
        // -----------------------------------------------------------------------
        event_loop.handle()
            .insert_source(Timer::immediate(), move |_, _, state| {
                let t0 = Instant::now();
                state.space.refresh();

                let elapsed = t0.duration_since(state.last_log_time).as_secs_f64();
                if elapsed >= 1.0 {
                    tracing::debug!(fps = state.encoded_frame_count as f64 / elapsed, "compositor fps");
                    state.encoded_frame_count = 0;
                    state.last_log_time = t0;
                }

                let has_peers = peer_count.as_ref()
                    .map_or(true, |c| c.load(std::sync::atomic::Ordering::Relaxed) > 0);

                if state.is_capturing && has_peers {
                    // Take damage_tracker out to avoid a simultaneous mutable borrow of `state`.
                    if let Some(mut dt) = state.damage_tracker.take() {
                        render_and_capture(state, &mut dt);
                        state.damage_tracker = Some(dt);
                    }
                } else if state.is_capturing && !has_peers {
                    // Log once per second so we can confirm idle mode is active.
                    if elapsed >= 1.0 {
                        tracing::debug!("Compositor idle: no peers, skipping render");
                    }
                }

                // Read pending clipboard data from the Wayland client that set the selection.
                // `pending_clipboard_mime` is set by `SelectionHandler::new_selection` and
                // consumed here — after the selection is committed to the seat data — so that
                // `request_data_device_client_selection` can find the active source.
                crate::compositor::check_pending_clipboard_read(state);

                let spent = t0.elapsed();
                if has_peers {
                    let frame_interval = Duration::from_secs_f64(1.0 / state.target_fps.max(1.0));
                    TimeoutAction::ToDuration(frame_interval.saturating_sub(spent))
                } else {
                    // No peers — sleep for 1 second between timer ticks instead of
                    // 1/fps so we're not spinning through space.refresh() at 30 Hz.
                    TimeoutAction::ToDuration(Duration::from_secs(1))
                }
            })
            .expect("Failed to insert frame timer");

        // -----------------------------------------------------------------------
        // Run
        // -----------------------------------------------------------------------
        while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            // When no peers are connected, back off to 100 ms so we're not
            // burning CPU on epoll + Wayland bookkeeping 250×/second.
            let has_peers = peer_count_loop.as_ref()
                .map_or(true, |c| c.load(std::sync::atomic::Ordering::Relaxed) > 0);
            let dispatch_timeout = if has_peers {
                Duration::from_millis(4)
            } else {
                Duration::from_millis(100)
            };
            event_loop.dispatch(Some(dispatch_timeout), &mut state)
                .context("Event loop dispatch error")?;
            state.space.refresh();
            state.popups.cleanup();
            if let Err(e) = state.dh.flush_clients() {
                tracing::warn!("Wayland flush error: {e}");
            }
        }

        tracing::info!("Compositor stopped.");
        Ok(())
    }
}

/// Apply a live resize to the compositor output.
///
/// Called from the calloop event loop when a `CompositorCommand::Resize` arrives.
/// Validates dimensions, updates state, rebuilds the offscreen buffer and damage tracker.
pub(crate) fn apply_resize(state: &mut AppState, w: u32, h: u32) {
    // Reject invalid dimensions (must be positive, even, within a sane limit).
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || w > 4096 || h > 4096 {
        tracing::warn!("Ignoring invalid resize request {}x{}", w, h);
        return;
    }
    if state.width == w as i32 && state.height == h as i32 {
        return; // no-op
    }

    tracing::info!("Compositor resizing {}x{} → {}x{}", state.width, state.height, w, h);

    state.width = w as i32;
    state.height = h as i32;

    // Resize the CPU frame buffer.
    if !state.use_gpu {
        state.frame_buffer.resize((w * h * 4) as usize, 0);
    }

    // Rebuild the GPU offscreen buffer at the new size.
    if state.use_gpu {
        if let Some(ref gbm) = state.gbm_device {
            match allocate_offscreen_ring(gbm, w, h) {
                Ok(ring) => {
                    state.offscreen_buffers = ring;
                    state.offscreen_index = 0;
                }
                Err(e) => {
                    tracing::error!("Failed to allocate offscreen ring buffers for resize: {e}");
                    return;
                }
            }
        }
    }

    // Update the Wayland output mode so clients see the new resolution.
    if let Some(output) = state.outputs.first() {
        #[allow(clippy::cast_possible_truncation)]
        let refresh_mhz = (state.target_fps * 1000.0).round() as i32;
        let new_mode = OutputMode { size: (w as i32, h as i32).into(), refresh: refresh_mhz };
        output.change_current_state(
            Some(new_mode),
            Some(smithay::utils::Transform::Normal),
            Some(smithay::output::Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output.set_preferred(new_mode);

        // Rebuild damage tracker for the new output geometry.
        state.damage_tracker = Some(OutputDamageTracker::from_output(output));
    }

    // Reconfigure every mapped XDG toplevel window at the new size.
    // Without this, Wayland clients keep their old dimensions and won't fill
    // the resized output.
    let windows: Vec<_> = state.space.elements().cloned().collect();
    for window in windows {
        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|pending| {
                use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
                pending.size = Some((w as i32, h as i32).into());
                pending.states.set(xdg_toplevel::State::Fullscreen);
                pending.states.set(xdg_toplevel::State::Activated);
            });
            toplevel.send_configure();
        }
    }
}

/// Set the compositor clipboard to `text`, making it available to Wayland clients that paste.
///
/// Stores the text in `AppState::clipboard_contents` (sent via `SelectionHandler::send_selection`)
/// and registers the compositor as the active clipboard owner via `set_data_device_selection`.
/// If a clipboard bridge is active, also forwards the text to the inner compositor.
pub(crate) fn apply_clipboard_write(state: &mut AppState, text: String) {
    tracing::debug!("apply_clipboard_write: {} bytes", text.len());
    use smithay::wayland::selection::data_device::set_data_device_selection;
    // Record the text before setting the selection so that if the Wayland client
    // echoes the selection back (common when the browser runs inside lumen), the
    // deduplication in check_pending_clipboard_read will suppress the echo.
    *state.clipboard_sent_text.lock().unwrap() = Some(text.clone());
    state.clipboard_contents = Some(text.clone());
    set_data_device_selection::<AppState>(
        &state.dh,
        &state.seat,
        vec![
            "text/plain;charset=utf-8".to_string(),
            "text/plain".to_string(),
            "UTF8_STRING".to_string(),
        ],
        (),
    );
    // Forward to the inner compositor so apps running inside it can paste.
    if let Some(ref tx) = state.bridge_write_tx {
        let _ = tx.try_send(text);
    }
}

/// Update the compositor's target frame rate.
///
/// Called from the calloop event loop when a `SetTargetFps` input event arrives.
/// Updates `AppState::target_fps` and the Wayland output's refresh rate so that
/// clients (e.g. screen-content sources) know the new rate. The frame timer
/// already reads `state.target_fps` each tick, so it picks up the new rate
/// automatically with no further changes needed.
pub(crate) fn apply_set_target_fps(state: &mut AppState, fps: f64) {
    let fps = fps.clamp(1.0, 240.0);
    if (state.target_fps - fps).abs() < 0.001 {
        return; // no-op
    }
    tracing::info!("Compositor target FPS: {} → {}", state.target_fps, fps);
    state.target_fps = fps;

    if let Some(output) = state.outputs.first() {
        #[allow(clippy::cast_possible_truncation)]
        let refresh_mhz = (fps * 1000.0).round() as i32;
        let current_mode = output.current_mode().unwrap_or(OutputMode {
            size: (state.width, state.height).into(),
            refresh: refresh_mhz,
        });
        let new_mode = OutputMode { size: current_mode.size, refresh: refresh_mhz };
        output.change_current_state(
            Some(new_mode),
            None,
            None,
            None,
        );
        output.set_preferred(new_mode);
    }
}

/// If a clipboard read is pending (set by `SelectionHandler::new_selection`), request the data
/// from the active Wayland client selection and spawn a thread to read it.
///
/// Called from the frame timer after the selection has been committed to seat data.
pub(crate) fn check_pending_clipboard_read(state: &mut AppState) {
    let mime_type = match state.pending_clipboard_mime.take() {
        Some(m) => m,
        None => return,
    };

    let (read_fd, write_fd) = match rustix::pipe::pipe() {
        Ok(fds) => fds,
        Err(e) => {
            tracing::warn!("Failed to create pipe for clipboard read: {e}");
            return;
        }
    };

    use smithay::wayland::selection::data_device::request_data_device_client_selection;
    tracing::debug!("check_pending_clipboard_read: requesting mime_type={}", mime_type);
    match request_data_device_client_selection::<AppState>(&state.seat, mime_type, write_fd) {
        Ok(()) => {
            let clipboard_tx = state.clipboard_tx.clone();
            let sent_text = state.clipboard_sent_text.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut text = String::new();
                // Blocking read: unblocks when the Wayland client writes data and closes its end.
                if std::fs::File::from(read_fd).read_to_string(&mut text).is_ok() && !text.is_empty() {
                    tracing::debug!("Clipboard read {} bytes", text.len());
                    // Deduplicate: if the text matches what we last broadcast, this is an echo
                    // (e.g. the browser running inside lumen reflecting our own clipboard_update
                    // back via wl_data_device::set_selection). Skip to break the feedback loop.
                    let mut guard = sent_text.lock().unwrap();
                    if guard.as_deref() == Some(text.as_str()) {
                        tracing::debug!("Clipboard dedup: skipping echo");
                        return;
                    }
                    *guard = Some(text.clone());
                    drop(guard);
                    tracing::debug!("Clipboard broadcasting text");
                    let _ = clipboard_tx.send(crate::types::ClipboardEvent::Text(text));
                } else {
                    tracing::debug!("Clipboard read empty or failed");
                }
            });
        }
        Err(e) => {
            tracing::debug!("Clipboard read skipped: {e}");
        }
    }
}

/// Number of DMA-BUF offscreen buffers to allocate for the render ring.
///
/// Three buffers give the VA-API encoder up to 3 frame-intervals (~100 ms at
/// 30 fps) to finish with a buffer before the compositor reuses it, preventing
/// GPU fence stalls on discrete GPUs where VA-API scheduling latency can spike
/// during idle power-state transitions.
const OFFSCREEN_RING_SIZE: usize = 3;

/// Allocate `OFFSCREEN_RING_SIZE` GBM offscreen buffers for GPU rendering.
///
/// Returns a `Vec` of `(BufferObject, Dmabuf handle, DRM modifier)` tuples.
fn allocate_offscreen_ring(
    gbm: &RawGbmDevice<std::fs::File>,
    width: u32,
    height: u32,
) -> anyhow::Result<Vec<(gbm::BufferObject<()>, smithay::backend::allocator::dmabuf::Dmabuf, u64)>> {
    let mut ring = Vec::with_capacity(OFFSCREEN_RING_SIZE);
    for i in 0..OFFSCREEN_RING_SIZE {
        let bo = gbm
            .create_buffer_object(width, height, GbmFormat::Argb8888, BufferObjectFlags::RENDERING)
            .with_context(|| format!("Failed to create GBM BO #{i} for offscreen ring"))?;
        let modifier = u64::from(bo.modifier());
        let dmabuf = crate::state::create_dmabuf_from_bo(&bo);
        ring.push((bo, dmabuf, modifier));
    }
    tracing::debug!("Allocated {} offscreen ring buffers ({}x{})", OFFSCREEN_RING_SIZE, width, height);
    Ok(ring)
}
