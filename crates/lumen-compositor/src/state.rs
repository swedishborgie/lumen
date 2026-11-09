use std::collections::HashSet;
use std::fs::File;
use std::hash::Hash;
use std::sync::{mpsc::SyncSender, Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
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
    // [diag]
    pub last_frame_tx_at: Option<Instant>,
    pub diag_capture_missed: u32,

    /// Current virtual cursor position in compositor logical space.
    /// Updated by absolute and relative pointer motion events. Used by
    /// `inject_pointer_motion_relative` to maintain a sane absolute position
    /// for surface-focus resolution alongside relative motion delivery.
    pub cursor_pos: Point<f64, Logical>,
}

/// Shared state across all `ClientState` instances for tracking
/// compositor clients (nested compositors like kwin) and triggering
/// a fatal exit if all of them disconnect.
///
/// When `launch_pid` is set, only clients whose PID is a descendant of that
/// process count.  This matters on NixOS: a previous lumen instance that was
/// killed (e.g. by systemd) leaks its whole desktop session — KDE's
/// session startup moves it into `user.slice`, out of lumen's cgroup.  The
/// leaked `kwin_wayland_wrapper` respawns a kwin that reconnects to the next
/// lumen instance; without the filter that stray kwin would cancel the
/// fatal-exit grace timer and keep a dead desktop alive indefinitely.
#[derive(Clone)]
pub struct CompositorClientTracker<C = smithay::reexports::wayland_server::backend::ClientId>
where
    C: Eq + Hash + Clone,
{
    /// Number of connected clients that have created compositor surfaces.
    count: Arc<AtomicUsize>,
    /// Set of client IDs that have been counted (i.e. created at least one
    /// compositor surface).  Protected by a mutex.
    tracked: Arc<Mutex<HashSet<C>>>,
    /// PID of the `--launch`/`--desktop` child.  `0` (unset) disables
    /// ancestry filtering and every surface-creating client counts.
    launch_pid: Arc<AtomicU32>,
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

impl<C: Eq + Hash + Clone> CompositorClientTracker<C> {
    /// Create a new tracker.  `shutdown_tx` is `None` when the feature is
    /// disabled; `launch_pid` is the shared atomic that the owning binary
    /// populates once the launch child is spawned.
    pub fn new(
        shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
        grace_ms: u64,
        launch_pid: Arc<AtomicU32>,
    ) -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(0)),
            tracked: Arc::new(Mutex::new(HashSet::new())),
            shutdown_tx: Arc::new(Mutex::new(shutdown_tx)),
            timer_thread: Arc::new(Mutex::new(None)),
            timer_active: Arc::new(AtomicBool::new(false)),
            grace_ms,
            launch_pid,
        }
    }

    /// Returns `true` when the client's PID belongs to the launch child's
    /// process subtree (or when ancestry filtering is disabled).
    pub fn is_launch_client(&self, client_pid: u32) -> bool {
        let launch_pid = self.launch_pid.load(Ordering::Relaxed);
        if launch_pid == 0 {
            return true; // filtering disabled
        }
        is_descendant_of(client_pid, launch_pid)
    }

    /// Number of currently tracked (connected) compositor clients.
    pub fn tracked_count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    /// `true` while the fatal-exit grace timer is running.
    pub fn grace_timer_active(&self) -> bool {
        self.timer_active.load(Ordering::Relaxed)
    }

    /// Called when a client creates its first compositor surface.
    /// Increments the count for launch-child clients; if a grace timer was
    /// active, cancels it.  Clients outside the launch subtree are ignored.
    pub fn client_created_surface(&self, client_id: C, client_pid: Option<u32>) {
        if let Some(pid) = client_pid {
            if !self.is_launch_client(pid) {
                tracing::warn!(
                    client_pid = pid,
                    launch_pid = self.launch_pid.load(Ordering::Relaxed),
                    "ignoring Wayland client outside the launch subtree (leaked session?)"
                );
                return;
            }
        }
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
    pub fn client_disconnected(&self, client_id: C) {
        // Only clients that were actually counted can start the grace timer;
        // everything else was ignored in `client_created_surface`.
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
    ///
    /// Returns `true` when the timer was actually armed.  When the feature
    /// is disabled or already fired (`shutdown_tx` absent), nothing is armed.
    fn start_grace_timer(&self) -> bool {
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

        let Some(shutdown_tx) = shutdown_tx else {
            // Feature was disabled or already fired.
            self.timer_active.store(false, Ordering::Relaxed);
            return false;
        };

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
                let _ = shutdown_tx.send(());
            })
            .expect("Failed to spawn grace timer thread");

        *self.timer_thread.lock().unwrap() = Some(thread);
        true
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

/// Returns `true` when `pid` is `ancestor` itself or any of its descendants.
///
/// Walks the parent chain via `/proc/<pid>/stat` (ppid, field 4).  Returns
/// `false` if the process is gone, unreadable, or the chain terminates without
/// reaching `ancestor`.  Bounded so a malformed chain cannot loop forever.
///
/// Linux-only, which matches lumen's deployment target.
pub fn is_descendant_of(pid: u32, ancestor: u32) -> bool {
    let mut current = pid;
    for _ in 0..512 {
        if current == ancestor {
            return true;
        }
        if current <= 1 {
            return false;
        }
        match read_ppid(current) {
            Some(ppid) => current = ppid,
            None => return false,
        }
    }
    false
}

/// Read the parent PID of `pid` from `/proc/<pid>/stat`.
///
/// The comm field (field 2) may contain spaces and parentheses, so parse from
/// the last `)` instead of splitting the whole line on whitespace.
pub fn read_ppid(pid: u32) -> Option<u32> {
    let data = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = data.rfind(')')? + 1;
    let fields: Vec<&str> = data[after_comm..].split_whitespace().collect();
    // Fields after ") ": state(0), ppid(1), …
    fields.get(1)?.parse().ok()
}

#[cfg(test)]
mod tracker_tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    /// Tracker keyed on plain `u32` client ids (the generic parameter exists
    /// so tests don't need to construct a real `ClientId`) with a real
    /// shutdown channel, so the grace timer can actually arm.  60 s grace is
    /// long enough that no test ever trips the real timeout.
    fn tracker(launch_pid: u32) -> CompositorClientTracker<u32> {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        CompositorClientTracker::new(Some(tx), 60_000, Arc::new(AtomicU32::new(launch_pid)))
    }

        #[test]
    fn descendant_check_walks_process_tree() {
        // `sh -c 'sleep 300 & echo $! > FILE; wait'`: the shell forks a real
        // `sleep` child (so it does NOT exec-optimize away) and blocks on
        // `wait`, keeping the whole tree alive for the duration of the check.
        // The child's pid is written to a unique file (a /proc scan is racy
        // under parallel tests).
        let pidfile = std::env::temp_dir().join(format!("lumen-desc-{}-{}", std::process::id(), std::thread::current().name().unwrap_or_default().rsplit("::").next().unwrap_or("t")));
        let pidfile_s = pidfile.to_string_lossy().to_string();
        let mut sh = std::process::Command::new("sh")
            .args(["-c", &format!("sleep 300 & echo $! > {pidfile_s}; wait")])
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("spawn shell");
        let sh_pid = sh.id();
        let self_pid = std::process::id();

        // Poll for the sleep grandchild's pid file.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut grandchild = None;
        while grandchild.is_none() && std::time::Instant::now() < deadline {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if let Ok(p) = s.trim().parse::<u32>() {
                    // It must actually be our child (guards against a stale file).
                    if read_ppid(p) == Some(sh_pid) {
                        grandchild = Some(p);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let grandchild = grandchild.expect("sleep grandchild of sh");
        assert!(
            is_descendant_of(grandchild, self_pid),
            "nested sleep must be recognised as a descendant of the test process"
        );
        assert!(is_descendant_of(sh_pid, self_pid));
        // Clean up: kill the whole tree.
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &sh_pid.to_string()])
            .status();
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &grandchild.to_string()])
            .status();
        let _ = sh.wait();
        let _ = std::fs::remove_file(pidfile);
    }

    #[test]
    fn descendant_check_rejects_unrelated_process() {
        let self_pid = std::process::id();
        // pid 1 (init) is an ANCESTOR of us, never a descendant.
        assert!(!is_descendant_of(1, self_pid), "pid 1 is not our descendant");
        // A nonexistent pid is not a descendant of anything.
        assert!(
            !is_descendant_of(999_999_999, self_pid),
            "nonexistent pid must not count as a descendant"
        );
        // A live, unrelated process (the test harness's parent) must not be
        // treated as our descendant either.
        let harn_ppid = read_ppid(self_pid).expect("own ppid");
        assert!(
            !is_descendant_of(harn_ppid, self_pid),
            "our parent is not our descendant (check must not invert)"
        );
    }

    #[test]
    fn tracker_counts_launch_descendant_only() {
        let self_pid = std::process::id();
        let t = tracker(self_pid);

        // `good_client` (client id 1) comes from the launch subtree — we use
        // the test pid itself as a proxy, since it is a descendant of itself.
        t.client_created_surface(1, Some(self_pid));
        // `stray_client` comes from a leaked session (pid 1 is never our child).
        t.client_created_surface(2, Some(1));

        assert!(t.is_launch_client(self_pid));
        assert!(!t.is_launch_client(1));
        assert_eq!(t.tracked_count(), 1, "only the launch client may be counted");

        // Dropping the stray client must not start the grace timer, and
        // dropping the real one must.
        t.client_disconnected(2);
        assert!(!t.grace_timer_active());
        t.client_disconnected(1);
        assert!(
            t.grace_timer_active(),
            "losing the last launch client must arm the grace timer"
        );
        // Clean up the timer thread.
        t.cancel_timer();
    }

    #[test]
    fn stray_client_cannot_cancel_grace_timer() {
        // The exact field failure: kwin crashes, its (leaked) wrapper respawns
        // a kwin that reconnects.  That respawned kwin is not in the launch
        // subtree, so it must NOT cancel the grace timer.
        let self_pid = std::process::id();
        let t = tracker(self_pid);
        t.client_created_surface(1, Some(self_pid));
        t.client_disconnected(1);
        assert!(t.grace_timer_active());
        // Stray kwin from the leaked session reconnects and makes a surface.
        t.client_created_surface(2, Some(1));
        assert!(
            t.grace_timer_active(),
            "stray client must not cancel the grace timer"
        );
        assert_eq!(t.tracked_count(), 0);
        // A genuine launch client reconnecting DOES cancel the timer.
        t.client_created_surface(3, Some(self_pid));
        assert!(!t.grace_timer_active(), "launch client reconnect must cancel the timer");
        t.cancel_timer();
    }

    #[test]
    fn reconnect_cancels_grace_timer() {
        let self_pid = std::process::id();
        let t = tracker(self_pid);
        t.client_created_surface(10, Some(self_pid));
        t.client_created_surface(11, Some(self_pid));
        t.client_disconnected(10);
        assert!(
            !t.grace_timer_active(),
            "timer only arms when the LAST launch client is lost"
        );
        t.client_disconnected(11);
        assert!(t.grace_timer_active());
        // A new launch client (kwin respawned by its wrapper) cancels it.
        t.client_created_surface(12, Some(self_pid));
        assert!(!t.grace_timer_active());
        t.cancel_timer();
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
    builder.add_plane(fd, 0, stride);
    builder.build().expect("Failed to build dmabuf")
}
