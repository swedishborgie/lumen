use std::fs::File;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gbm::{BufferObjectFlags, Device as RawGbmDevice, Format as GbmFormat};
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
use crate::types::{CapturedFrame, CompositorConfig, CursorEvent};

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
}

/// Smithay-based Wayland compositor.
pub struct Compositor {
    config: CompositorConfig,
    frame_tx: broadcast::Sender<CapturedFrame>,
    cursor_tx: broadcast::Sender<CursorEvent>,
    cmd_tx: smithay::reexports::calloop::channel::Sender<CompositorCommand>,
    cmd_rx: Option<smithay::reexports::calloop::channel::Channel<CompositorCommand>>,
}

impl Compositor {
    pub fn new(config: CompositorConfig) -> Result<Self> {
        let (frame_tx, _) = broadcast::channel(8);
        let (cursor_tx, _) = broadcast::channel(16);
        let (cmd_tx, cmd_rx) = smithay::reexports::calloop::channel::channel();
        Ok(Self { config, frame_tx, cursor_tx, cmd_tx, cmd_rx: Some(cmd_rx) })
    }

    pub fn input_sender(&self) -> InputSender { InputSender(self.cmd_tx.clone()) }
    pub fn frame_receiver(&self) -> broadcast::Receiver<CapturedFrame> { self.frame_tx.subscribe() }
    pub fn cursor_receiver(&self) -> broadcast::Receiver<CursorEvent> { self.cursor_tx.subscribe() }
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
        let mut offscreen_buffer = None;
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
            let bo = gbm_alloc
                .create_buffer_object(width.cast_unsigned(), height.cast_unsigned(), GbmFormat::Argb8888, BufferObjectFlags::RENDERING)
                .context("Failed to create GBM BO")?;
            let dmabuf = crate::state::create_dmabuf_from_bo(&bo);
            offscreen_buffer = Some((bo, dmabuf));
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
            outputs: Vec::new(), pending_windows: Vec::new(), foreign_toplevel_list,
            xdg_decoration_state, xdg_activation_state, primary_selection_state, popups,
            frame_buffer: vec![0u8; usize::try_from(width * height * 4).expect("frame buffer size fits usize")],
            gles_renderer, pixman_renderer, gbm_device: gbm_device_raw, offscreen_buffer,
            is_capturing: true, width, height, target_fps, frame_tx, cursor_tx,
            frame_counter: 0, clock: Clock::new(), current_cursor_icon: None,
            last_log_time: Instant::now(), encoded_frame_count: 0, start_time: Instant::now(),
            use_gpu,
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
        let mut damage_tracker = OutputDamageTracker::from_output(&output);

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
                        crate::input::inject_input(state, ev);
                    }
                    CalloopEvent::Closed => {}
                }
            })
            .expect("Failed to insert command channel source");

        // -----------------------------------------------------------------------
        // Frame timer
        // -----------------------------------------------------------------------
        let frame_interval = Duration::from_secs_f64(1.0 / target_fps);
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

                if state.is_capturing {
                    render_and_capture(state, &mut damage_tracker);
                }

                let spent = t0.elapsed();
                TimeoutAction::ToDuration(frame_interval.saturating_sub(spent))
            })
            .expect("Failed to insert frame timer");

        // -----------------------------------------------------------------------
        // Run
        // -----------------------------------------------------------------------
        while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            event_loop.dispatch(Some(Duration::from_millis(4)), &mut state)
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
