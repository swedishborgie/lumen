use std::collections::HashSet;
use std::fs::File;
use std::sync::{mpsc::SyncSender, Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use gbm::{BufferObject, Device as RawGbmDevice};
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{
            damage::OutputDamageTracker,
            gles::GlesRenderer,
            pixman::PixmanRenderer,
        },
    },
    desktop::{PopupManager, Space, Window},
    input::{
        pointer::CursorImageStatus,
        Seat, SeatState,
    },
    output::Output,
    reexports::wayland_server::DisplayHandle,
    utils::{Clock, Logical, Monotonic, Point},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufGlobal, DmabufState},
        fractional_scale::FractionalScaleManagerState,
        foreign_toplevel_list::ForeignToplevelListState,
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        pointer_warp::PointerWarpManager,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        cursor_shape::CursorShapeManagerState,
        selection::{
            data_device::DataDeviceState,
            primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        shell::{wlr_layer::WlrLayerShellState, xdg::XdgShellState, xdg::decoration::XdgDecorationState},
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        virtual_keyboard::VirtualKeyboardManagerState,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
    },
};
use tokio::sync::broadcast;

use crate::input::InputEvent;
use crate::types::{CapturedFrame, ClipboardEvent, CursorEvent};

/// Internal commands sent into the calloop event loop.
pub enum CompositorCommand {
    Input(InputEvent),
    Resize(u32, u32),
    ClipboardWrite(String),
    Stop,
}

/// All Smithay state in one struct, owned by the calloop event loop.
pub struct AppState {
    pub compositor_state: CompositorState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub viewporter_state: ViewporterState,
    pub presentation_state: PresentationState,
    pub shm_state: ShmState,
    pub single_pixel_buffer: SinglePixelBufferState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub output_state: OutputManagerState,
    pub seat_state: SeatState<AppState>,
    pub shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub data_device_state: DataDeviceState,
    pub data_control_state: DataControlState,
    pub virtual_keyboard_state: VirtualKeyboardManagerState,
    pub pointer_warp_state: PointerWarpManager,
    pub relative_pointer_state: RelativePointerManagerState,
    pub pointer_constraints_state: PointerConstraintsState,
    pub cursor_shape_state: CursorShapeManagerState,
    pub foreign_toplevel_list: ForeignToplevelListState,
    pub xdg_decoration_state: XdgDecorationState,
    pub xdg_activation_state: XdgActivationState,
    pub primary_selection_state: PrimarySelectionState,
    pub popups: PopupManager,

    pub dh: DisplayHandle,
    pub seat: Seat<AppState>,
    pub outputs: Vec<Output>,
    pub space: Space<Window>,
    pub pending_windows: Vec<Window>,

    pub gles_renderer: Option<GlesRenderer>,
    pub pixman_renderer: Option<PixmanRenderer>,
    pub gbm_device: Option<RawGbmDevice<File>>,
    /// Ring of offscreen GBM buffers used for GPU rendering.
    ///
    /// Using multiple buffers (triple-buffering) prevents the compositor from
    /// stalling on `GlesRenderer::bind()` while waiting for a DRM implicit
    /// fence to be released by the VA-API encoder.  On a discrete GPU (e.g.
    /// RX 5700 XT via PCIe), `amdgpu` idle power-state transitions can delay
    /// fence signals by 1-2 seconds, causing dropped frames.  With a ring of
    /// N buffers the compositor always renders into a buffer that was last used
    /// ≥ N-1 frames ago, giving the encoder ample time to finish.
    ///
    /// Each entry is `(GBM buffer object, Smithay Dmabuf handle, DRM modifier)`.
    pub offscreen_buffers: Vec<(BufferObject<()>, Dmabuf, u64)>,
    /// Index into `offscreen_buffers` pointing at the slot used for the
    /// most-recently rendered frame.  Incremented (mod ring size) every frame.
    pub offscreen_index: usize,
    pub use_gpu: bool,
    pub frame_buffer: Vec<u8>,

    /// Damage tracker, stored here so the resize handler can rebuild it.
    pub damage_tracker: Option<OutputDamageTracker>,

    pub is_capturing: bool,
    pub width: i32,
    pub height: i32,
    pub target_fps: f64,
    pub frame_tx: broadcast::Sender<CapturedFrame>,
    pub cursor_tx: broadcast::Sender<CursorEvent>,
    pub clipboard_tx: broadcast::Sender<ClipboardEvent>,
    pub frame_counter: u64,
    pub clock: Clock<Monotonic>,

    pub current_cursor_icon: Option<CursorImageStatus>,

    /// Text most recently set as compositor-owned clipboard content.
    /// Written to Wayland clients via `SelectionHandler::send_selection`.
    pub clipboard_contents: Option<String>,
    /// Text MIME type of a pending clipboard read request (set by `new_selection`).
    /// Consumed in the frame timer to request data from the Wayland client.
    pub pending_clipboard_mime: Option<String>,
    /// Last clipboard text successfully broadcast to the frontend.
    /// Shared with background reader threads to deduplicate and break feedback loops.
    pub clipboard_sent_text: Arc<std::sync::Mutex<Option<String>>>,
    /// Sender to the clipboard bridge task (if `--inner-display` is configured).
    /// `apply_clipboard_write` pushes text here so the bridge can forward it to the
    /// inner compositor via `zwlr_data_control_manager_v1`.
    pub bridge_write_tx: Option<SyncSender<String>>,

    pub last_log_time: Instant,
    pub encoded_frame_count: u32,
    pub start_time: Instant,

    /// Current virtual cursor position in compositor logical space.
    /// Updated by absolute and relative pointer motion events. Used by
    /// `inject_pointer_motion_relative` to maintain a sane absolute position
    /// for surface-focus resolution alongside relative motion delivery.
    pub cursor_pos: Point<f64, Logical>,
}

/// Shared state across all `ClientState` instances for tracking
/// compositor clients (nested compositors like kwin) and triggering
/// a fatal exit if all of them disconnect.
#[derive(Clone)]
pub struct CompositorClientTracker {
    /// Number of connected clients that have created compositor surfaces.
    count: Arc<AtomicUsize>,
    /// Set of client IDs that have been counted (i.e. created at least one
    /// compositor surface).  Protected by a mutex.
    tracked: Arc<Mutex<HashSet<smithay::reexports::wayland_server::backend::ClientId>>>,
    /// `oneshot::Sender` to fire when all compositor clients are lost after
    /// the grace period has elapsed.
    shutdown_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    /// Handle to the grace-period timer thread.  `Some` means the timer
    /// is active (waiting to fire); `None` means no timer is running.
    timer_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// Whether the timer is currently active (used for lock-free cancel checks).
    timer_active: Arc<AtomicBool>,
    /// Grace period in milliseconds.
    grace_ms: u64,
}

impl CompositorClientTracker {
    /// Create a new tracker.  `shutdown_tx` is `None` when the feature is disabled.
    pub fn new(
        shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
        grace_ms: u64,
    ) -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(0)),
            tracked: Arc::new(Mutex::new(HashSet::new())),
            shutdown_tx: Arc::new(Mutex::new(shutdown_tx)),
            timer_thread: Arc::new(Mutex::new(None)),
            timer_active: Arc::new(AtomicBool::new(false)),
            grace_ms,
        }
    }

    /// Called when a client creates its first compositor surface.
    /// Increments the count; if a grace timer was active, cancels it.
    pub fn client_created_surface(
        &self,
        client_id: smithay::reexports::wayland_server::backend::ClientId,
    ) {
        // Cancel any in-flight grace timer before incrementing.
        self.cancel_timer();

        let mut tracked = self.tracked.lock().unwrap();
        if tracked.insert(client_id) {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Called when a client disconnects.
    /// If the client was tracked, decrements the count.  When the count
    /// reaches zero, starts the grace-period timer.
    pub fn client_disconnected(
        &self,
        client_id: smithay::reexports::wayland_server::backend::ClientId,
    ) {
        let mut tracked = self.tracked.lock().unwrap();
        if tracked.remove(&client_id) {
            let prev = self.count.fetch_sub(1, Ordering::Relaxed);
            assert!(prev > 0, "compositor client count underflow");
            if prev == 1 {
                // This was the last compositor client — start the grace timer.
                drop(tracked);
                self.start_grace_timer();
            }
        }
    }

    /// Start a background thread that sleeps for `grace_ms`, then sends
    /// on the shutdown channel and exits.  Can be cancelled via `cancel_timer()`.
    fn start_grace_timer(&self) {
        tracing::warn!(
            grace_ms = self.grace_ms,
            "All compositor clients disconnected — starting {}ms grace period before fatal exit",
            self.grace_ms,
        );

        self.timer_active.store(true, Ordering::Relaxed);

        let shutdown_tx = {
            let mut tx = self.shutdown_tx.lock().unwrap();
            tx.take()
        };

        if shutdown_tx.is_none() {
            // Feature was disabled or already fired.
            self.timer_active.store(false, Ordering::Relaxed);
            return;
        }

        let grace_ms = self.grace_ms;
        let timer_active = Arc::clone(&self.timer_active);
        let thread = std::thread::Builder::new()
            .name("compositor-grace-timer".into())
            .spawn(move || {
                // Sleep in 100ms steps so we can respond to cancel quickly.
                let steps = grace_ms / 100;
                let remainder = grace_ms % 100;
                for _ in 0..steps {
                    if !timer_active.load(Ordering::Relaxed) {
                        tracing::debug!("compositor grace timer cancelled — new client connected");
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if remainder > 0 {
                    if !timer_active.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(remainder));
                }
                // Grace period elapsed — no new compositor client appeared.
                tracing::error!(
                    "Grace period elapsed with no compositor clients — exiting so systemd can restart"
                );
                if let Some(tx) = shutdown_tx {
                    let _ = tx.send(());
                }
            })
            .expect("Failed to spawn grace timer thread");

        *self.timer_thread.lock().unwrap() = Some(thread);
    }

    /// Cancel the grace-period timer (called when a new compositor client appears).
    fn cancel_timer(&self) {
        if self.timer_active.compare_exchange(
            true,
            false,
            Ordering::Relaxed,
            Ordering::Relaxed,
        )
        .is_ok()
        {
            // We successfully cancelled — the timer thread will notice and exit
            // on its next 100ms check.  Join it when it's ready.
            if let Some(thread) = self.timer_thread.lock().unwrap().take() {
                let _ = thread.join();
            }
        }
    }
}

/// Per-client data stored by the Wayland server.
pub struct ClientState {
    pub compositor_client_state: smithay::wayland::compositor::CompositorClientState,
    /// Shared tracker for fatal-exit-on-compositor-loss.
    pub tracker: CompositorClientTracker,
}

impl smithay::reexports::wayland_server::backend::ClientData for ClientState {
    fn initialized(
        &self,
        client_id: smithay::reexports::wayland_server::backend::ClientId,
    ) {
        tracing::info!("Wayland client connected: {:?}", client_id);
        // If a grace timer is active, a new client connecting might become
        // a compositor client.  We don't cancel the timer here — we wait
        // for `client_created_surface` which is the definitive signal.
    }
    fn disconnected(
        &self,
        client_id: smithay::reexports::wayland_server::backend::ClientId,
        reason: smithay::reexports::wayland_server::backend::DisconnectReason,
    ) {
        tracing::info!("Wayland client disconnected: {:?} reason={:?}", client_id, reason);
        self.tracker.client_disconnected(client_id);
    }
}

/// Build a `Dmabuf` from a GBM buffer object.
pub(crate) fn create_dmabuf_from_bo(bo: &BufferObject<()>) -> Dmabuf {
    use smithay::backend::allocator::{dmabuf::DmabufFlags, Fourcc, Modifier};

    let fd = bo.fd().expect("Failed to get GBM BO fd");
    let width = bo.width() as i32;
    let height = bo.height() as i32;
    let stride = bo.stride();
    let modifier = Modifier::from(u64::from(bo.modifier()));

    let mut builder = Dmabuf::builder(
        (width, height),
        Fourcc::Argb8888,
        modifier,
        DmabufFlags::empty(),
    );
    builder.add_plane(fd, 0, 0, stride);
    builder.build().expect("Failed to build dmabuf")
}
