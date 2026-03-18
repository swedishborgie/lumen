//! Clipboard bridge between Lumen and a nested inner compositor.
//!
//! When Lumen nests another compositor (e.g. labwc, KWin), clipboard events from apps running
//! inside that nested compositor never reach Lumen's `SelectionHandler` because the inner
//! compositor owns its clipboard domain privately.
//!
//! This module connects to the inner compositor's Wayland socket **as a client** and uses
//! either `ext_data_control_manager_v1` (preferred, KWin/KDE Plasma 6+) or the older
//! `zwlr_data_control_manager_v1` (labwc, older compositors) to bridge clipboard
//! bidirectionally:
//!
//! - **Inner → Outer**: `selection` events on the inner socket → read pipe → broadcast
//!   `ClipboardEvent::Text` through Lumen's existing channel.
//! - **Outer → Inner**: When the browser pastes (via `ClipboardWrite`), we receive the text
//!   on `write_rx` and advertise it on the inner socket via a `data_control_source`.

use std::{
    io::{Read, Write},
    os::unix::{io::AsFd, net::UnixStream},
    sync::{
        mpsc::{Receiver, TryRecvError},
        Arc, Mutex,
    },
    time::Duration,
};

use tokio::sync::broadcast;
use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry, wl_seat::WlSeat},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

use crate::types::ClipboardEvent;

// ── Protocol-agnostic wrappers ────────────────────────────────────────────────

/// Which data-control protocol the inner compositor speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    /// `ext_data_control_manager_v1` — standardized, used by KWin/KDE Plasma 6+.
    Ext,
    /// `zwlr_data_control_manager_v1` — wlroots-specific, used by labwc/sway/wlroots.
    Zwlr,
}

/// Wraps either an ext or zwlr data-control offer.
enum AnyOffer {
    Ext(ExtDataControlOfferV1),
    Zwlr(ZwlrDataControlOfferV1),
}

impl AnyOffer {
    fn matches(&self, other: &AnyOffer) -> bool {
        match (self, other) {
            (AnyOffer::Ext(a), AnyOffer::Ext(b)) => a.id() == b.id(),
            (AnyOffer::Zwlr(a), AnyOffer::Zwlr(b)) => a.id() == b.id(),
            _ => false,
        }
    }

    fn receive(&self, mime: String, fd: rustix::fd::BorrowedFd<'_>) {
        match self {
            AnyOffer::Ext(o) => o.receive(mime, fd),
            AnyOffer::Zwlr(o) => o.receive(mime, fd),
        }
    }

    fn destroy(self) {
        match self {
            AnyOffer::Ext(o) => o.destroy(),
            AnyOffer::Zwlr(o) => o.destroy(),
        }
    }
}

impl std::fmt::Debug for AnyOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnyOffer::Ext(o) => write!(f, "Ext({:?})", o.id()),
            AnyOffer::Zwlr(o) => write!(f, "Zwlr({:?})", o.id()),
        }
    }
}

/// Wraps either an ext or zwlr data-control source.
enum AnySource {
    Ext(ExtDataControlSourceV1),
    Zwlr(ZwlrDataControlSourceV1),
}

impl AnySource {
    fn offer(&self, mime: String) {
        match self {
            AnySource::Ext(s) => s.offer(mime),
            AnySource::Zwlr(s) => s.offer(mime),
        }
    }

    fn destroy(self) {
        match self {
            AnySource::Ext(s) => s.destroy(),
            AnySource::Zwlr(s) => s.destroy(),
        }
    }
}

/// Wraps either an ext or zwlr data-control device.
enum AnyDevice {
    Ext(ExtDataControlDeviceV1),
    Zwlr(ZwlrDataControlDeviceV1),
}

impl AnyDevice {
    fn set_selection(&self, source: Option<&AnySource>) {
        match (self, source) {
            (AnyDevice::Ext(d), Some(AnySource::Ext(s))) => d.set_selection(Some(s)),
            (AnyDevice::Ext(d), None) => d.set_selection(None),
            (AnyDevice::Zwlr(d), Some(AnySource::Zwlr(s))) => d.set_selection(Some(s)),
            (AnyDevice::Zwlr(d), None) => d.set_selection(None),
            _ => tracing::warn!("clipboard_bridge: protocol mismatch in set_selection"),
        }
    }
}

/// Wraps either an ext or zwlr manager; creates sources of the matching type.
enum AnyManager {
    Ext(ExtDataControlManagerV1),
    Zwlr(ZwlrDataControlManagerV1),
}

impl AnyManager {
    fn create_source(&self, qh: &QueueHandle<BridgeState>) -> AnySource {
        match self {
            AnyManager::Ext(m) => AnySource::Ext(m.create_data_source(qh, ())),
            AnyManager::Zwlr(m) => AnySource::Zwlr(m.create_data_source(qh, ())),
        }
    }
}

// ── Internal state held in the wayland-client dispatch loop ──────────────────

struct BridgeState {
    /// Sender for broadcasting clipboard events back to Lumen's async world.
    clipboard_tx: broadcast::Sender<ClipboardEvent>,
    /// Tracks the last text broadcast or received, for echo-loop suppression.
    clipboard_sent_text: Arc<Mutex<Option<String>>>,
    /// The data-control device bound to seat0.
    device: Option<AnyDevice>,
    /// Current clipboard offer (if any) from the inner compositor.
    current_offer: Option<AnyOffer>,
    /// MIME types advertised by the current clipboard offer.
    offer_mime_types: Vec<String>,
    /// The active outbound source we created (if any). Held alive until replaced or dropped.
    active_source: Option<AnySource>,
    /// Text we are currently advertising via `active_source`.
    active_source_text: Option<String>,
}

impl BridgeState {
    fn new(
        clipboard_tx: broadcast::Sender<ClipboardEvent>,
        clipboard_sent_text: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            clipboard_tx,
            clipboard_sent_text,
            device: None,
            current_offer: None,
            offer_mime_types: Vec::new(),
            active_source: None,
            active_source_text: None,
        }
    }
}

// ── Dispatch implementations ──────────────────────────────────────────────────

// wl_registry: only used during setup to bind the global interfaces.
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for BridgeState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Handled by registry_queue_init.
    }
}

delegate_noop!(BridgeState: ignore WlSeat);
delegate_noop!(BridgeState: ignore ZwlrDataControlManagerV1);
delegate_noop!(BridgeState: ignore ExtDataControlManagerV1);

// ── zwlr dispatch impls ───────────────────────────────────────────────────────

impl Dispatch<ZwlrDataControlOfferV1, ()> for BridgeState {
    fn event(
        state: &mut Self,
        offer: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            if state.current_offer.as_ref().map(|o| matches!(o, AnyOffer::Zwlr(z) if z.id() == offer.id())).unwrap_or(false) {
                state.offer_mime_types.push(mime_type);
            }
        }
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for BridgeState {
    fn event_created_child(opcode: u16, qhandle: &QueueHandle<Self>) -> std::sync::Arc<dyn wayland_client::backend::ObjectData> {
        if opcode == 0 {
            qhandle.make_data::<ZwlrDataControlOfferV1, _>(())
        } else {
            tracing::warn!(
                "clipboard_bridge: ZwlrDataControlDeviceV1 event_created_child opcode {opcode} — \
                 using noop fallback"
            );
            qhandle.make_data::<ZwlrDataControlOfferV1, _>(())
        }
    }

    fn event(
        state: &mut Self,
        _device: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        handle_device_event_zwlr(state, event);
    }
}

fn handle_device_event_zwlr(state: &mut BridgeState, event: zwlr_data_control_device_v1::Event) {
    match event {
        zwlr_data_control_device_v1::Event::DataOffer { id } => {
            tracing::debug!("clipboard_bridge: [zwlr] DataOffer {:?}", id);
            if let Some(old) = state.current_offer.take() {
                old.destroy();
            }
            state.offer_mime_types.clear();
            state.current_offer = Some(AnyOffer::Zwlr(id));
        }
        zwlr_data_control_device_v1::Event::Selection { id } => {
            let matches = id.as_ref().map(|o| {
                state.current_offer.as_ref().map(|c| c.matches(&AnyOffer::Zwlr(o.clone()))).unwrap_or(false)
            }).unwrap_or(false);
            tracing::debug!("clipboard_bridge: [zwlr] Selection {:?} (matches current: {})", id, matches);
            handle_selection(state, id.map(AnyOffer::Zwlr));
        }
        zwlr_data_control_device_v1::Event::Finished => {
            tracing::debug!("clipboard_bridge: [zwlr] data_control_device finished");
        }
        _ => {
            tracing::debug!("clipboard_bridge: [zwlr] unhandled device event (likely primary_selection)");
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for BridgeState {
    fn event(
        state: &mut Self,
        _source: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { mime_type, fd } => {
                handle_source_send(state, mime_type, fd);
            }
            zwlr_data_control_source_v1::Event::Cancelled => {
                handle_source_cancelled(state);
            }
            _ => {}
        }
    }
}

// ── ext dispatch impls ────────────────────────────────────────────────────────

impl Dispatch<ExtDataControlOfferV1, ()> for BridgeState {
    fn event(
        state: &mut Self,
        offer: &ExtDataControlOfferV1,
        event: ext_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            if state.current_offer.as_ref().map(|o| matches!(o, AnyOffer::Ext(e) if e.id() == offer.id())).unwrap_or(false) {
                state.offer_mime_types.push(mime_type);
            }
        }
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for BridgeState {
    fn event_created_child(opcode: u16, qhandle: &QueueHandle<Self>) -> std::sync::Arc<dyn wayland_client::backend::ObjectData> {
        // Opcode 0 = data_offer (creates ExtDataControlOfferV1).
        if opcode == 0 {
            qhandle.make_data::<ExtDataControlOfferV1, _>(())
        } else {
            tracing::warn!(
                "clipboard_bridge: ExtDataControlDeviceV1 event_created_child opcode {opcode} — \
                 using noop fallback"
            );
            qhandle.make_data::<ExtDataControlOfferV1, _>(())
        }
    }

    fn event(
        state: &mut Self,
        _device: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        handle_device_event_ext(state, event);
    }
}

fn handle_device_event_ext(state: &mut BridgeState, event: ext_data_control_device_v1::Event) {
    match event {
        ext_data_control_device_v1::Event::DataOffer { id } => {
            tracing::debug!("clipboard_bridge: [ext] DataOffer {:?}", id);
            if let Some(old) = state.current_offer.take() {
                old.destroy();
            }
            state.offer_mime_types.clear();
            state.current_offer = Some(AnyOffer::Ext(id));
        }
        ext_data_control_device_v1::Event::Selection { id } => {
            let matches = id.as_ref().map(|o| {
                state.current_offer.as_ref().map(|c| c.matches(&AnyOffer::Ext(o.clone()))).unwrap_or(false)
            }).unwrap_or(false);
            tracing::debug!("clipboard_bridge: [ext] Selection {:?} (matches current: {})", id, matches);
            handle_selection(state, id.map(AnyOffer::Ext));
        }
        ext_data_control_device_v1::Event::Finished => {
            tracing::debug!("clipboard_bridge: [ext] data_control_device finished");
        }
        _ => {
            tracing::debug!("clipboard_bridge: [ext] unhandled device event (likely primary_selection)");
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for BridgeState {
    fn event(
        state: &mut Self,
        _source: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_source_v1::Event::Send { mime_type, fd } => {
                handle_source_send(state, mime_type, fd);
            }
            ext_data_control_source_v1::Event::Cancelled => {
                handle_source_cancelled(state);
            }
            _ => {}
        }
    }
}

// ── Shared event handlers ─────────────────────────────────────────────────────

fn handle_selection(state: &mut BridgeState, offer: Option<AnyOffer>) {
    if let Some(offer_obj) = offer {
        if state.current_offer.as_ref().map(|o| o.matches(&offer_obj)).unwrap_or(false) {
            let mime_types = std::mem::take(&mut state.offer_mime_types);
            let clipboard_tx = state.clipboard_tx.clone();
            let sent_text = Arc::clone(&state.clipboard_sent_text);
            try_read_offer(offer_obj, mime_types, clipboard_tx, sent_text);
            state.current_offer = None;
        } else {
            tracing::warn!(
                "clipboard_bridge: Selection offer {:?} does not match current_offer {:?} — \
                 discarding (possible primary-selection interleaving)",
                offer_obj,
                state.current_offer
            );
            offer_obj.destroy();
            state.current_offer = None;
            state.offer_mime_types.clear();
        }
    } else {
        // Clipboard cleared.
        if let Some(old) = state.current_offer.take() {
            old.destroy();
        }
        state.offer_mime_types.clear();
        let _ = state.clipboard_tx.send(ClipboardEvent::Cleared);
    }
}

fn handle_source_send(
    state: &mut BridgeState,
    mime_type: String,
    fd: rustix::fd::OwnedFd,
) {
    if let Some(ref text) = state.active_source_text {
        let data = if mime_type.starts_with("text/")
            || mime_type == "UTF8_STRING"
            || mime_type == "STRING"
            || mime_type == "TEXT"
        {
            text.as_bytes().to_vec()
        } else {
            return;
        };
        // Write in a background thread so we don't block the dispatch loop.
        std::thread::spawn(move || {
            let _ = std::fs::File::from(fd).write_all(&data);
        });
    }
}

fn handle_source_cancelled(state: &mut BridgeState) {
    tracing::debug!("clipboard_bridge: outbound source cancelled");
    if let Some(src) = state.active_source.take() {
        src.destroy();
    }
    state.active_source_text = None;
}

// ── Offer reading helper ───────────────────────────────────────────────────────

/// Spawn a thread to read clipboard text from the offer's pipe and broadcast it.
fn try_read_offer(
    offer: AnyOffer,
    mime_types: Vec<String>,
    clipboard_tx: broadcast::Sender<ClipboardEvent>,
    sent_text: Arc<Mutex<Option<String>>>,
) {
    let preferred = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"];
    let mime = match preferred.iter().find(|m| mime_types.contains(&m.to_string())) {
        Some(m) => m.to_string(),
        None => {
            tracing::debug!("clipboard_bridge: no text MIME in offer ({:?}), skipping", mime_types);
            offer.destroy();
            return;
        }
    };

    let (read_fd, write_fd) = match rustix::pipe::pipe() {
        Ok(fds) => fds,
        Err(e) => {
            tracing::warn!("clipboard_bridge: failed to create pipe: {e}");
            offer.destroy();
            return;
        }
    };

    offer.receive(mime, write_fd.as_fd());
    drop(write_fd); // close our end so the inner compositor's write unblocks
    offer.destroy();

    std::thread::spawn(move || {
        let mut text = String::new();
        if std::fs::File::from(read_fd).read_to_string(&mut text).is_ok() && !text.is_empty() {
            tracing::debug!("clipboard_bridge: received {} bytes from inner compositor", text.len());
            let mut guard = sent_text.lock().unwrap();
            if guard.as_deref() == Some(text.as_str()) {
                tracing::debug!("clipboard_bridge: dedup — skipping echo from inner compositor");
                return;
            }
            *guard = Some(text.clone());
            drop(guard);
            tracing::debug!("clipboard_bridge: broadcasting ClipboardEvent::Text");
            let _ = clipboard_tx.send(ClipboardEvent::Text(text));
        } else {
            tracing::debug!("clipboard_bridge: offer read empty or failed");
        }
    });
}

// ── Socket auto-discovery ─────────────────────────────────────────────────────

/// Scan `$XDG_RUNTIME_DIR` for Wayland socket files and return the name of the
/// first one (other than `skip_socket`) that advertises either
/// `ext_data_control_manager_v1` or `zwlr_data_control_manager_v1`.
///
/// Returns `None` if no suitable socket is found.
fn discover_inner_socket(skip_socket: &str) -> Option<String> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", rustix::process::getuid().as_raw()));
    let dir = std::path::Path::new(&runtime_dir);

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return None,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Only consider wayland-* socket files.
        if !name_str.starts_with("wayland-") || name_str.ends_with(".lock") {
            continue;
        }
        let candidate = name_str.into_owned();
        // Skip lumen's own socket and the outer host compositor's socket.
        if candidate == skip_socket {
            continue;
        }
        let socket_path = dir.join(&candidate);
        // Probe: try connecting and see if either data-control protocol is available.
        if probe_has_data_control(&socket_path).is_some() {
            tracing::info!("clipboard_bridge: discovered inner socket {:?}", candidate);
            return Some(candidate);
        }
    }
    None
}

/// Return which data-control protocol the socket at `path` advertises, or `None`.
/// Prefers `ext_data_control_manager_v1` over `zwlr_data_control_manager_v1`.
fn probe_has_data_control(path: &std::path::Path) -> Option<Protocol> {
    let stream = match UnixStream::connect(path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("clipboard_bridge: probe {:?} — connect failed: {e}", path);
            return None;
        }
    };
    let conn = match Connection::from_socket(stream) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("clipboard_bridge: probe {:?} — connection setup failed: {e}", path);
            return None;
        }
    };
    struct ProbeState;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ProbeState {
        fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event,
            _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    let result = registry_queue_init::<ProbeState>(&conn);
    match result {
        Ok((globals, _)) => {
            globals.contents().with_list(|list| {
                tracing::debug!(
                    "clipboard_bridge: probe {:?} — globals: [{}]",
                    path,
                    list.iter()
                        .map(|g| format!("{} v{}", g.interface, g.version))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let has_ext = list.iter().any(|g| g.interface == "ext_data_control_manager_v1");
                let has_zwlr = list.iter().any(|g| g.interface == "zwlr_data_control_manager_v1");
                if has_ext {
                    tracing::debug!("clipboard_bridge: probe {:?} — using ext_data_control_manager_v1", path);
                    Some(Protocol::Ext)
                } else if has_zwlr {
                    tracing::debug!("clipboard_bridge: probe {:?} — using zwlr_data_control_manager_v1", path);
                    Some(Protocol::Zwlr)
                } else {
                    tracing::debug!("clipboard_bridge: probe {:?} — no data-control protocol found", path);
                    None
                }
            })
        }
        Err(e) => {
            tracing::debug!("clipboard_bridge: probe {:?} — registry init failed: {e}", path);
            None
        }
    }
}

// ── Bridge entry point ────────────────────────────────────────────────────────

/// Run the clipboard bridge. Intended to be called from a dedicated OS thread.
///
/// `inner_display` is either:
/// - A specific Wayland socket name (e.g. `"wayland-2"`), or
/// - `"auto"` to auto-discover the inner compositor by scanning `$XDG_RUNTIME_DIR`
///   for a socket that advertises `ext_data_control_manager_v1` or
///   `zwlr_data_control_manager_v1`.
///
/// - Incoming clipboard changes are broadcast via `clipboard_tx`.
/// - Text received on `write_rx` is pushed to the inner compositor as a new selection.
/// - Retries the connection on failure, waiting briefly between attempts.
pub fn run(
    inner_display: String,
    clipboard_tx: broadcast::Sender<ClipboardEvent>,
    clipboard_sent_text: Arc<Mutex<Option<String>>>,
    write_rx: Receiver<String>,
) {
    if inner_display.is_empty() {
        tracing::debug!("clipboard_bridge: disabled (inner_display is empty)");
        return;
    }
    // `lumen_socket` is the socket name lumen itself is listening on — we skip it
    // during auto-discovery so we don't accidentally bridge to ourselves.
    let lumen_socket = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let auto = inner_display == "auto";
    tracing::info!(
        "clipboard_bridge: starting (inner_display={:?}, auto={})",
        inner_display, auto
    );

    loop {
        let target = if auto {
            match discover_inner_socket(&lumen_socket) {
                Some(name) => name,
                None => {
                    tracing::debug!("clipboard_bridge: no inner compositor found yet, retrying in 2s");
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        } else {
            inner_display.clone()
        };

        match try_connect_and_run(&target, &clipboard_tx, &clipboard_sent_text, &write_rx) {
            Ok(()) => {
                tracing::info!("clipboard_bridge: inner compositor disconnected, stopping");
                break;
            }
            Err(e) => {
                tracing::warn!("clipboard_bridge: connection error ({target:?}): {e}; retrying in 2s");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

fn try_connect_and_run(
    inner_display: &str,
    clipboard_tx: &broadcast::Sender<ClipboardEvent>,
    clipboard_sent_text: &Arc<Mutex<Option<String>>>,
    write_rx: &Receiver<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", rustix::process::getuid().as_raw()));
    let socket_path = std::path::Path::new(&runtime_dir).join(inner_display);
    let stream = UnixStream::connect(&socket_path)
        .map_err(|e| format!("cannot connect to {}: {e}", socket_path.display()))?;
    let conn = Connection::from_socket(stream)?;
    tracing::info!("clipboard_bridge: connected to {:?}", inner_display);

    let (globals, mut queue): (_, EventQueue<BridgeState>) = registry_queue_init(&conn)?;
    let qh = queue.handle();

    // Log all globals and detect which data-control protocol is available.
    let protocol = globals.contents().with_list(|list| {
        tracing::debug!(
            "clipboard_bridge: inner compositor globals: [{}]",
            list.iter()
                .map(|g| format!("{} v{}", g.interface, g.version))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let has_ext = list.iter().any(|g| g.interface == "ext_data_control_manager_v1");
        let has_zwlr = list.iter().any(|g| g.interface == "zwlr_data_control_manager_v1");
        if has_ext { Some(Protocol::Ext) } else if has_zwlr { Some(Protocol::Zwlr) } else { None }
    });

    let protocol = protocol.ok_or_else(|| {
        "neither ext_data_control_manager_v1 nor zwlr_data_control_manager_v1 advertised".to_string()
    })?;

    tracing::info!("clipboard_bridge: using protocol {:?}", protocol);

    // Bind wl_seat (needed by both protocols to create a data-control device).
    let seat: WlSeat = globals
        .bind(&qh, 1..=9, ())
        .map_err(|e| format!("wl_seat not available: {e}"))?;

    let manager: AnyManager;
    let device: AnyDevice;

    match protocol {
        Protocol::Ext => {
            let mgr: ExtDataControlManagerV1 = globals
                .bind(&qh, 1..=1, ())
                .map_err(|e| format!("ext_data_control_manager_v1 bind failed: {e}"))?;
            let dev = mgr.get_data_device(&seat, &qh, ());
            device = AnyDevice::Ext(dev);
            manager = AnyManager::Ext(mgr);
        }
        Protocol::Zwlr => {
            let mgr: ZwlrDataControlManagerV1 = globals
                .bind(&qh, 1..=2, ())
                .map_err(|e| format!("zwlr_data_control_manager_v1 bind failed: {e}"))?;
            let dev = mgr.get_data_device(&seat, &qh, ());
            device = AnyDevice::Zwlr(dev);
            manager = AnyManager::Zwlr(mgr);
        }
    }

    let mut state = BridgeState::new(clipboard_tx.clone(), Arc::clone(clipboard_sent_text));
    state.device = Some(device);

    // Initial roundtrip to receive the current clipboard selection.
    queue.roundtrip(&mut state)?;

    tracing::debug!("clipboard_bridge: dispatch loop running");

    loop {
        // Dispatch any pending Wayland events (non-blocking).
        queue.dispatch_pending(&mut state)?;

        // Flush outbound messages.
        conn.flush()?;

        // Check for clipboard text to push to the inner compositor.
        match write_rx.try_recv() {
            Ok(text) => {
                tracing::debug!("clipboard_bridge: advertising {} bytes to inner compositor", text.len());

                // Replace any existing source.
                if let Some(old) = state.active_source.take() {
                    old.destroy();
                }

                let source = manager.create_source(&qh);
                source.offer("text/plain;charset=utf-8".into());
                source.offer("text/plain".into());
                source.offer("UTF8_STRING".into());

                state.device.as_ref().map(|d| d.set_selection(Some(&source)));

                // Update dedup guard so we don't echo this back.
                *clipboard_sent_text.lock().unwrap() = Some(text.clone());

                state.active_source_text = Some(text);
                state.active_source = Some(source);

                // Flush the set_selection immediately.
                conn.flush()?;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                tracing::info!("clipboard_bridge: write channel closed, stopping");
                return Ok(());
            }
        }

        // Wait up to 10ms for events before polling write_rx again.
        let read_guard = queue.prepare_read();
        if let Some(guard) = read_guard {
            let fd = guard.connection_fd();
            let timeout = rustix::time::Timespec { tv_sec: 0, tv_nsec: 10_000_000 };
            let mut pollfd = [rustix::event::PollFd::new(
                &fd,
                rustix::event::PollFlags::IN,
            )];
            let _ = rustix::event::poll(&mut pollfd, Some(&timeout));
            if pollfd[0].revents().contains(rustix::event::PollFlags::IN) {
                guard.read()?;
            }
        }
    }
}
