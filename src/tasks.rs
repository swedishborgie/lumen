use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use base64::Engine as _;
use tokio::sync::broadcast;

// ── Event serialization ───────────────────────────────────────────────────────

/// Encode a `CursorEvent` as a JSON byte string for the data channel.
pub fn cursor_event_to_json(ev: &lumen_compositor::CursorEvent) -> Vec<u8> {
    use lumen_compositor::CursorEvent;
    let json = match ev {
        CursorEvent::Default => br#"{"type":"cursor_update","kind":"default"}"#.to_vec(),
        CursorEvent::Named(css) => {
            format!(r#"{{"type":"cursor_update","kind":"named","css":"{css}"}}"#).into_bytes()
        }
        CursorEvent::Hidden => br#"{"type":"cursor_update","kind":"hidden"}"#.to_vec(),
        CursorEvent::Image { width, height, hotspot_x, hotspot_y, rgba } => {
            let data = base64::engine::general_purpose::STANDARD.encode(rgba);
            format!(
                r#"{{"type":"cursor_update","kind":"image","w":{width},"h":{height},"hotspot_x":{hotspot_x},"hotspot_y":{hotspot_y},"data":"{data}"}}"#
            ).into_bytes()
        }
    };
    tracing::debug!("cursor -> browser: {}", String::from_utf8_lossy(&json).chars().take(120).collect::<String>());
    json
}

/// Encode a `ClipboardEvent` as a JSON byte string for the data channel.
/// Returns `None` for `Cleared` (no meaningful data to send to the browser).
pub fn clipboard_event_to_json(ev: &lumen_compositor::ClipboardEvent) -> Option<Vec<u8>> {
    use lumen_compositor::ClipboardEvent;
    match ev {
        ClipboardEvent::Text(text) => {
            let text_json = serde_json::to_string(text).unwrap_or_default();
            Some(format!(r#"{{"type":"clipboard_update","text":{text_json}}}"#).into_bytes())
        }
        ClipboardEvent::Cleared => None,
    }
}

// ── Blocking thread spawns ────────────────────────────────────────────────────

/// Spawn the compositor on a dedicated thread.
pub fn spawn_compositor(mut compositor: lumen_compositor::Compositor) {
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(
            std::panic::AssertUnwindSafe(|| compositor.run()),
        );
        match result {
            Ok(Ok(())) => {
                // Normal shutdown: Stop command was received and the event loop
                // exited cleanly. Main will call std::process::exit(0) shortly.
            }
            Ok(Err(e)) => {
                tracing::error!("Compositor crashed: {e:#}; exiting so systemd can restart");
                std::process::exit(1);
            }
            Err(_) => {
                tracing::error!("Compositor panicked; exiting so systemd can restart");
                std::process::exit(1);
            }
        }
    });
}

/// Spawn the audio capture loop on a blocking task.
pub fn spawn_audio(mut audio_capture: lumen_audio::AudioCapture) {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = audio_capture.run() {
            tracing::error!("Audio capture: {e:#}");
        }
    });
}

/// Spawn the encoder loop on a blocking task.
pub fn spawn_encoder(
    encoder_config: lumen_encode::EncoderConfig,
    frame_rx: broadcast::Receiver<lumen_compositor::CapturedFrame>,
    encoded_tx: broadcast::Sender<Arc<lumen_encode::EncodedFrame>>,
    keyframe_flag: Arc<AtomicBool>,
    enc_resize_rx: std::sync::mpsc::Receiver<(u32, u32)>,
    peer_count: Arc<AtomicUsize>,
    metrics_tx: Option<tokio::sync::watch::Sender<lumen_web::metrics::EncoderMetrics>>,
) {
    tokio::task::spawn_blocking(move || {
        let mut encoder = match lumen_encode::create_encoder(&encoder_config) {
            Ok(e) => e,
            Err(e) => { tracing::error!("Encoder init: {e:#}"); return; }
        };
        let mut frame_rx = frame_rx;
        let mut encoded_count: u64 = 0;
        let mut dropped_count: u64 = 0;
        let mut encoder_width = encoder_config.width;
        let mut encoder_height = encoder_config.height;
        let frame_interval = std::time::Duration::from_secs_f64(1.0 / encoder_config.fps);
        loop {
            // Check for a pending resize before blocking on the next frame.
            match enc_resize_rx.try_recv() {
                Ok((w, h)) => {
                    tracing::info!("Encoder resizing to {w}x{h}");
                    match encoder.resize(w, h) {
                        Ok(()) => { encoder_width = w; encoder_height = h; }
                        Err(e) => tracing::error!("Encoder resize failed: {e:#}"),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }

            let frame = match frame_rx.blocking_recv() {
                Ok(f) => f,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    dropped_count += n;
                    continue;
                }
                Err(_) => break,
            };
            // Warn when a frame arrives with abnormally high capture-to-encode latency.
            // This indicates the compositor or encoder pipeline is stalling — on a
            // discrete GPU this is often caused by GPU fence contention on the DMA-BUF.
            let capture_latency = frame.captured_at.elapsed();
            if capture_latency > frame_interval * 2 {
                tracing::warn!(
                    ms = capture_latency.as_millis(),
                    "Frame arrived at encoder with high latency — possible GPU fence stall or compositor backpressure"
                );
            }
            // Skip encoding when nobody is watching.
            if peer_count.load(Ordering::Relaxed) == 0 {
                continue;
            }
            // Skip frames from the old resolution that arrived after a resize.
            if frame.width != encoder_width || frame.height != encoder_height {
                continue;
            }
            // Service any pending keyframe request before encoding.
            if keyframe_flag.swap(false, Ordering::Relaxed) {
                encoder.request_keyframe();
            }
            let encode_start = std::time::Instant::now();
            match encoder.encode(frame) {
                Ok(Some(ef)) => {
                    let encode_time_us = encode_start.elapsed().as_micros() as u64;
                    encoded_count += 1;
                    if let Some(ref tx) = metrics_tx {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let _ = tx.send(lumen_web::metrics::EncoderMetrics {
                            timestamp_ms: now_ms,
                            frames_encoded: encoded_count,
                            encode_time_us,
                            capture_latency_us: capture_latency.as_micros() as u64,
                            frame_size_bytes: ef.data.len(),
                            keyframe: ef.is_keyframe,
                            dropped_frames: dropped_count,
                            width: encoder_width,
                            height: encoder_height,
                        });
                    }
                    let _ = encoded_tx.send(Arc::new(ef));
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("Encode error: {e:#}"),
            }
        }
    });
}

/// Spawn the gamepad manager on a blocking task.
///
/// Runs in `spawn_blocking` because uinput file descriptor writes are
/// synchronous.  A channel bridges the async world to the blocking loop.
///
/// FF (force-feedback) events are polled at ~60 Hz; when an application
/// plays a rumble effect the resulting `HapticCommand` is forwarded to
/// `haptic_tx` so it can be relayed back to the browser.
pub fn spawn_gamepad_manager(
    gamepad_rx: tokio::sync::mpsc::Receiver<lumen_gamepad::GamepadEvent>,
    haptic_tx: tokio::sync::mpsc::Sender<(u8, lumen_gamepad::HapticCommand)>,
) {
    let mut rx = gamepad_rx;
    tokio::task::spawn_blocking(move || {
        let handle = tokio::runtime::Handle::current();
        let poll_interval = std::time::Duration::from_millis(16);
        let mut manager = lumen_gamepad::GamepadManager::new();
        loop {
            // Block for up to 16 ms waiting for the next gamepad input event.
            // The timeout ensures we poll FF events regularly even when the
            // browser isn't actively pressing buttons.
            let recv = handle.block_on(
                tokio::time::timeout(poll_interval, rx.recv())
            );

            match recv {
                // A gamepad input event arrived within the timeout.
                Ok(Some(ev)) => manager.handle_event(ev),
                // Channel closed — exit.
                Ok(None) => {
                    tracing::debug!("Gamepad manager: channel closed, exiting");
                    break;
                }
                // Timeout elapsed with no input — just fall through to FF poll.
                Err(_) => {}
            }

            // Poll force-feedback events from all connected virtual devices.
            for (index, cmd) in manager.poll_haptic_commands() {
                if haptic_tx.blocking_send((index, cmd)).is_err() {
                    tracing::debug!("Gamepad haptic channel closed");
                }
            }
        }
    });
}

/// Spawn the haptic fan-out task.
///
/// Receives `(gamepad_index, HapticCommand)` from the gamepad manager,
/// serialises each command as a JSON data channel message, and broadcasts it
/// to all active WebRTC sessions.
pub fn spawn_haptic_fanout(
    mut haptic_rx: tokio::sync::mpsc::Receiver<(u8, lumen_gamepad::HapticCommand)>,
    session_manager: Arc<lumen_webrtc::SessionManager>,
) {
    tokio::spawn(async move {
        while let Some((index, cmd)) = haptic_rx.recv().await {
            let json = format!(
                r#"{{"type":"haptic","index":{index},"strong_magnitude":{:.6},"weak_magnitude":{:.6},"duration_ms":{}}}"#,
                cmd.strong_magnitude,
                cmd.weak_magnitude,
                cmd.duration_ms,
            ).into_bytes();
            tracing::debug!(
                index,
                strong_magnitude = cmd.strong_magnitude,
                weak_magnitude = cmd.weak_magnitude,
                duration_ms = cmd.duration_ms,
                "haptic -> browser"
            );
            session_manager.broadcast_dc_message(json).await;
        }
    });
}

/// Spawn the child process launch task (for `--launch` / `--desktop`).
///
/// Waits for the compositor socket to be ready, then starts the child.
/// When the child exits it sends on `shutdown_tx` to stop the web server.
///
/// Returns the `shutdown_tx` sender when no launch command is configured, so
/// the caller must keep the returned value alive to prevent an early shutdown.
pub fn spawn_launch_task(
    effective_launch: Option<(String, &'static [(&'static str, &'static str)])>,
    socket_name_rx: std::sync::mpsc::Receiver<String>,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
) -> Option<tokio::sync::oneshot::Sender<()>> {
    if let Some((launch_cmd, preset_env)) = effective_launch {
        tokio::task::spawn_blocking(move || {
            let socket_name = match socket_name_rx.recv() {
                Ok(s) => s,
                Err(_) => {
                    tracing::error!("launch: compositor exited before socket was ready");
                    let _ = shutdown_tx.send(());
                    return;
                }
            };
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .unwrap_or_else(|_| "/tmp".to_string());
            tracing::info!(cmd = %launch_cmd, wayland_display = %socket_name, "Launching client");
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.args(["-c", &launch_cmd])
                .env("WAYLAND_DISPLAY", &socket_name)
                .env("XDG_RUNTIME_DIR", &runtime_dir)
                .env_remove("DISPLAY");
            // Strip all LUMEN_ vars so secrets don't leak into the child process.
            for (key, _) in std::env::vars() {
                if key.starts_with("LUMEN_") {
                    cmd.env_remove(&key);
                }
            }
            for (key, val) in preset_env {
                cmd.env(key, val);
            }
            match cmd.status() {
                Ok(s) => tracing::info!(cmd = %launch_cmd, status = %s, "Launched client exited; shutting down"),
                Err(e) => tracing::error!(cmd = %launch_cmd, "Failed to launch client: {e:#}"),
            }
            let _ = shutdown_tx.send(());
        });
        // shutdown_tx was moved into the task; no keep-alive needed
        None
    } else {
        // No launch command — keep the sender alive so the server runs forever.
        Some(shutdown_tx)
    }
}

// ── Async fan-out tasks ───────────────────────────────────────────────────────

/// Spawn the input forwarding task.
///
/// Routes `InputEvent`s from the web layer to either the compositor (keyboard,
/// mouse) or the gamepad manager (gamepad events).
pub fn spawn_input_forwarder(
    mut input_rx: tokio::sync::mpsc::Receiver<lumen_compositor::InputEvent>,
    compositor_input_tx: lumen_compositor::InputSender,
    gamepad_tx: tokio::sync::mpsc::Sender<lumen_gamepad::GamepadEvent>,
) {
    tokio::spawn(async move {
        while let Some(ev) = input_rx.recv().await {
            match ev {
                lumen_compositor::InputEvent::ClipboardWrite { text } => {
                    compositor_input_tx.clipboard_write(text);
                }
                lumen_compositor::InputEvent::GamepadConnected { index, name, mapping, buttons, axes } => {
                    tracing::debug!("Routing GamepadConnected: index={index} name={name:?} mapping={mapping:?}");
                    let gp_buttons = buttons.map(|bs| bs.into_iter().map(|b| b.map(|b| lumen_gamepad::ButtonMapping {
                        btn_code: b.btn_code,
                        trigger_abs_code: b.trigger_abs_code,
                    })).collect());
                    let gp_axes = axes.map(|ax| ax.into_iter().map(|a| a.map(|a| lumen_gamepad::AxisMapping {
                        abs_code: a.abs_code,
                    })).collect());
                    if gamepad_tx.send(lumen_gamepad::GamepadEvent::Connected {
                        index, name, mapping, buttons: gp_buttons, axes: gp_axes,
                    }).await.is_err() {
                        tracing::warn!("Gamepad manager channel closed; dropping GamepadConnected for index={index}");
                    }
                }
                lumen_compositor::InputEvent::GamepadDisconnected { index } => {
                    tracing::debug!("Routing GamepadDisconnected: index={index}");
                    let _ = gamepad_tx.send(lumen_gamepad::GamepadEvent::Disconnected { index }).await;
                }
                lumen_compositor::InputEvent::GamepadButton { index, button, value, pressed } => {
                    let _ = gamepad_tx.send(lumen_gamepad::GamepadEvent::Button {
                        index, button, value, pressed,
                    }).await;
                }
                lumen_compositor::InputEvent::GamepadAxis { index, axis, value } => {
                    let _ = gamepad_tx.send(lumen_gamepad::GamepadEvent::Axis {
                        index, axis, value,
                    }).await;
                }
                other => {
                    compositor_input_tx.send(other);
                }
            }
        }
    });
}

/// Spawn the resize coordinator task.
///
/// Receives `(width, height)` from the web layer, fans the command out to the
/// compositor and the encoder, then triggers a keyframe at the new size.
pub fn spawn_resize_coordinator(
    mut resize_rx: tokio::sync::mpsc::Receiver<(u32, u32)>,
    compositor_input_tx: lumen_compositor::InputSender,
    keyframe_flag: Arc<AtomicBool>,
    enc_resize_tx: std::sync::mpsc::Sender<(u32, u32)>,
) {
    tokio::spawn(async move {
        while let Some((w, h)) = resize_rx.recv().await {
            tracing::info!("Resize requested: {w}x{h}");
            compositor_input_tx.resize(w, h);
            let _ = enc_resize_tx.send((w, h));
            keyframe_flag.store(true, Ordering::Relaxed);
        }
    });
}

/// Spawn the audio fan-out task.
///
/// Distributes each `OpusPacket` to all active WebRTC sessions.
pub fn spawn_audio_fanout(
    mut audio_rx: tokio::sync::mpsc::Receiver<lumen_audio::OpusPacket>,
    session_manager: Arc<lumen_webrtc::SessionManager>,
) {
    tokio::spawn(async move {
        loop {
            let packet = match audio_rx.recv().await {
                Some(p) => p,
                None => break,
            };
            let sessions = session_manager.all_sessions().await;
            for session in sessions {
                let mut s = session.lock().await;
                if let Err(e) = s.push_audio(&packet) {
                    tracing::debug!("Audio push error: {e:#}");
                }
            }
        }
    });
}

/// Spawn the video fan-out task.
///
/// Distributes each encoded H.264 frame to all active WebRTC sessions.
pub fn spawn_video_fanout(
    encoded_tx: broadcast::Sender<Arc<lumen_encode::EncodedFrame>>,
    session_manager: Arc<lumen_webrtc::SessionManager>,
) {
    let mut encoded_rx = encoded_tx.subscribe();
    tokio::spawn(async move {
        loop {
            let frame = match encoded_rx.recv().await {
                Ok(f) => f,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Video fan-out lagged {n}");
                    continue;
                }
                Err(_) => break,
            };
            let sessions = session_manager.all_sessions().await;
            for session in sessions {
                let mut s = session.lock().await;
                if let Err(e) = s.push_video(&frame) {
                    tracing::debug!("Video push error: {e:#}");
                }
            }
            // Yield to the tokio scheduler so each drive task gets a chance to
            // flush the frame we just pushed into str0m before the next frame
            // arrives.  Without this the fan-out loop can immediately receive
            // and push the next frame before any drive task runs, causing two
            // frames to accumulate in str0m's pacer queue and be sent as a
            // burst — producing the 59ms/0ms double-frame pattern visible in
            // push_video interval logs and perceived as a stall by Firefox.
            tokio::task::yield_now().await;
        }
    });
}

/// Spawn the cursor fan-out task.
///
/// Converts each `CursorEvent` to JSON, caches the latest, and broadcasts it
/// to all active WebRTC sessions via the data channel.
///
/// Returns the shared cache so the web server can replay it to new connections.
pub fn spawn_cursor_fanout(
    mut cursor_rx: broadcast::Receiver<lumen_compositor::CursorEvent>,
    session_manager: Arc<lumen_webrtc::SessionManager>,
) -> Arc<tokio::sync::Mutex<Option<Vec<u8>>>> {
    let last_cursor_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let last_cursor_json_clone = last_cursor_json.clone();
    tokio::spawn(async move {
        loop {
            let ev = match cursor_rx.recv().await {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let json = cursor_event_to_json(&ev);
            *last_cursor_json_clone.lock().await = Some(json.clone());
            session_manager.broadcast_dc_message(json).await;
        }
    });
    last_cursor_json
}

/// Spawn the clipboard fan-out task.
///
/// Converts each `ClipboardEvent` to JSON, caches the latest, and broadcasts
/// it to all active WebRTC sessions via the data channel.
///
/// Returns the shared cache so the web server can replay it to new connections.
pub fn spawn_clipboard_fanout(
    mut clipboard_rx: broadcast::Receiver<lumen_compositor::ClipboardEvent>,
    session_manager: Arc<lumen_webrtc::SessionManager>,
) -> Arc<tokio::sync::Mutex<Option<Vec<u8>>>> {
    let last_clipboard_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let last_clipboard_json_clone = last_clipboard_json.clone();
    tokio::spawn(async move {
        loop {
            let ev = match clipboard_rx.recv().await {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let json_opt = clipboard_event_to_json(&ev);
            *last_clipboard_json_clone.lock().await = json_opt.clone();
            if let Some(json) = json_opt {
                session_manager.broadcast_dc_message(json).await;
            }
        }
    });
    last_clipboard_json
}

/// Spawn the system monitor task.
///
/// Samples `/proc/stat` and `/proc/meminfo` every second and publishes the
/// results into a `watch` channel.  The signaling layer reads from the
/// corresponding receiver when a browser has the performance overlay open.
///
/// Returns a `watch::Receiver` for the latest `SystemMetrics` snapshot, or
/// `None` if `/proc/stat` is not available (non-Linux platforms).
pub fn spawn_system_monitor() -> Option<tokio::sync::watch::Receiver<lumen_web::metrics::SystemMetrics>> {
    // Pre-check: if /proc/stat doesn't exist (non-Linux), skip.
    if !std::path::Path::new("/proc/stat").exists() {
        return None;
    }

    let (tx, rx) = tokio::sync::watch::channel(lumen_web::metrics::SystemMetrics::default());

    tokio::spawn(async move {
        // Previous /proc/stat CPU totals for delta calculation.
        let mut prev_idle: u64 = 0;
        let mut prev_total: u64 = 0;
        let mut first = true;

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            let cpu_pct = read_cpu_usage(&mut prev_idle, &mut prev_total, first);
            first = false;

            let (mem_used, mem_total) = read_mem_info();

            let _ = tx.send(lumen_web::metrics::SystemMetrics {
                cpu_usage_pct: cpu_pct,
                mem_used_mb: mem_used,
                mem_total_mb: mem_total,
            });
        }
    });

    Some(rx)
}

/// Parse `/proc/stat` and return the all-core CPU utilization percentage since
/// the previous call.  Returns `None` on the first call (no delta yet) or on
/// parse error.
fn read_cpu_usage(prev_idle: &mut u64, prev_total: &mut u64, first: bool) -> Option<f32> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() < 4 {
        return None;
    }
    // Fields: user, nice, system, idle, iowait, irq, softirq, steal, ...
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    let total: u64 = nums.iter().sum();

    let d_total = total.saturating_sub(*prev_total);
    let d_idle  = idle.saturating_sub(*prev_idle);

    *prev_total = total;
    *prev_idle  = idle;

    if first || d_total == 0 {
        return None;
    }

    let busy = d_total.saturating_sub(d_idle);
    Some((busy as f32 / d_total as f32) * 100.0)
}

/// Parse `/proc/meminfo` and return `(used_mb, total_mb)`, both `None` on
/// parse error.  "Used" is defined as `MemTotal - MemAvailable`.
fn read_mem_info() -> (Option<u32>, Option<u32>) {
    let info = match std::fs::read_to_string("/proc/meminfo") {
        Ok(s) => s,
        Err(_) => return (None, None),
    };
    let mut total_kb: Option<u64> = None;
    let mut avail_kb: Option<u64> = None;
    for line in info.lines() {
        if line.starts_with("MemTotal:") {
            total_kb = line.split_whitespace().nth(1).and_then(|v| v.parse().ok());
        } else if line.starts_with("MemAvailable:") {
            avail_kb = line.split_whitespace().nth(1).and_then(|v| v.parse().ok());
        }
        if total_kb.is_some() && avail_kb.is_some() {
            break;
        }
    }
    match (total_kb, avail_kb) {
        (Some(t), Some(a)) => {
            let used = t.saturating_sub(a);
            (Some((used / 1024) as u32), Some((t / 1024) as u32))
        }
        _ => (None, None),
    }
}
