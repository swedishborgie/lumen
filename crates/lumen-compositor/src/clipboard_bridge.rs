//! Clipboard bridge between Lumen and a nested inner compositor.
//!
//! When Lumen nests another compositor (e.g. Weston), clipboard events from apps running
//! inside that nested compositor never reach Lumen's `SelectionHandler` because Weston owns
//! its clipboard domain privately.
//!
//! This module connects to the inner compositor's Wayland socket **as a client** and uses
//! `zwlr_data_control_manager_v1` to bridge clipboard bidirectionally:
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
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

use crate::types::ClipboardEvent;

// ── Internal state held in the wayland-client dispatch loop ──────────────────

struct BridgeState {
    /// Sender for broadcasting clipboard events back to Lumen's async world.
    clipboard_tx: broadcast::Sender<ClipboardEvent>,
    /// Tracks the last text broadcast or received, for echo-loop suppression.
    clipboard_sent_text: Arc<Mutex<Option<String>>>,
    /// The data-control device bound to seat0.
    device: Option<ZwlrDataControlDeviceV1>,
    /// Current clipboard offer (if any) from the inner compositor.
    current_offer: Option<ZwlrDataControlOfferV1>,
    /// MIME types advertised by the current offer, in preference order.
    offer_mime_types: Vec<String>,
    /// The active outbound source we created (if any). Held alive until replaced or dropped.
    active_source: Option<ZwlrDataControlSourceV1>,
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

// ZwlrDataControlOfferV1: accumulates the MIME types advertised by the inner clipboard.
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
            // Only accumulate MIME types for the current pending offer.
            if state.current_offer.as_ref().map(|o| o == offer).unwrap_or(false) {
                state.offer_mime_types.push(mime_type);
            }
        }
    }
}

// ZwlrDataControlDeviceV1: receives clipboard change notifications from the inner compositor.
impl Dispatch<ZwlrDataControlDeviceV1, ()> for BridgeState {
    fn event_created_child(opcode: u16, qhandle: &QueueHandle<Self>) -> std::sync::Arc<dyn wayland_client::backend::ObjectData> {
        // Opcode 0 = data_offer, which creates a ZwlrDataControlOfferV1 child.
        if opcode == 0 {
            qhandle.make_data::<ZwlrDataControlOfferV1, _>(())
        } else {
            panic!("unexpected event_created_child for ZwlrDataControlDeviceV1 opcode {opcode}")
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
        match event {
            zwlr_data_control_device_v1::Event::DataOffer { id } => {
                // A new offer object is being constructed; MIME types follow.
                if let Some(old) = state.current_offer.take() {
                    old.destroy();
                }
                state.offer_mime_types.clear();
                state.current_offer = Some(id);
            }
            zwlr_data_control_device_v1::Event::Selection { id } => {
                // `id` is the offer that is now the active clipboard.
                if let Some(offer_obj) = id {
                    // Confirm this is the offer we've been accumulating MIME types for.
                    if state.current_offer.as_ref().map(|o| o == &offer_obj).unwrap_or(false) {
                        let mime_types = std::mem::take(&mut state.offer_mime_types);
                        let clipboard_tx = state.clipboard_tx.clone();
                        let sent_text = Arc::clone(&state.clipboard_sent_text);
                        try_read_offer(offer_obj, mime_types, clipboard_tx, sent_text);
                        state.current_offer = None;
                    } else {
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
            zwlr_data_control_device_v1::Event::Finished => {
                tracing::debug!("clipboard_bridge: data_control_device finished");
            }
            _ => {}
        }
    }
}

// ZwlrDataControlSourceV1: handles paste requests from apps running in the inner compositor.
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
                if let Some(ref text) = state.active_source_text {
                    let data = if mime_type.starts_with("text/") || mime_type == "UTF8_STRING"
                        || mime_type == "STRING" || mime_type == "TEXT"
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
            zwlr_data_control_source_v1::Event::Cancelled => {
                // Another client took ownership of the clipboard; release our source.
                tracing::debug!("clipboard_bridge: outbound source cancelled");
                if let Some(src) = state.active_source.take() {
                    src.destroy();
                }
                state.active_source_text = None;
            }
            _ => {}
        }
    }
}

// ── Offer reading helper ───────────────────────────────────────────────────────

/// Spawn a thread to read clipboard text from the offer's pipe and broadcast it.
fn try_read_offer(
    offer: ZwlrDataControlOfferV1,
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
/// first one (other than `skip_socket`) that advertises `zwlr_data_control_manager_v1`.
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
        // Probe: try connecting and see if zwlr_data_control_manager_v1 is available.
        if probe_has_data_control(&socket_path) {
            tracing::info!("clipboard_bridge: discovered inner socket {:?}", candidate);
            return Some(candidate);
        }
    }
    None
}

/// Return `true` if the Wayland socket at `path` advertises `zwlr_data_control_manager_v1`.
fn probe_has_data_control(path: &std::path::Path) -> bool {
    let stream = match UnixStream::connect(path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let conn = match Connection::from_socket(stream) {
        Ok(c) => c,
        Err(_) => return false,
    };
    struct ProbeState;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ProbeState {
        fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event,
            _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    let result = registry_queue_init::<ProbeState>(&conn);
    match result {
        Ok((globals, _)) => globals.contents().with_list(|list| {
            list.iter().any(|g| g.interface == "zwlr_data_control_manager_v1")
        }),
        Err(_) => false,
    }
}

// ── Bridge entry point ────────────────────────────────────────────────────────

/// Run the clipboard bridge. Intended to be called from a dedicated OS thread.
///
/// `inner_display` is either:
/// - A specific Wayland socket name (e.g. `"wayland-2"`), or
/// - `"auto"` to auto-discover the inner compositor by scanning `$XDG_RUNTIME_DIR`
///   for a socket that advertises `zwlr_data_control_manager_v1`.
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

    // Bind required globals.
    let manager: ZwlrDataControlManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|e| format!("zwlr_data_control_manager_v1 not available: {e}"))?;

    let seat: WlSeat = globals
        .bind(&qh, 7..=8, ())
        .map_err(|e| format!("wl_seat not available: {e}"))?;

    let mut state = BridgeState::new(clipboard_tx.clone(), Arc::clone(clipboard_sent_text));

    // Create the data-control device for seat0.
    let device = manager.get_data_device(&seat, &qh, ());
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

                let source = manager.create_data_source(&qh, ());
                source.offer("text/plain;charset=utf-8".into());
                source.offer("text/plain".into());
                source.offer("UTF8_STRING".into());

                if let Some(ref dev) = state.device {
                    dev.set_selection(Some(&source));
                }

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
