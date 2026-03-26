# Lumen — Agent Instructions

See [`dev-docs/`](./dev-docs/README.md) for full documentation. Key references:

- [Architecture](./dev-docs/architecture.md)
- [Component design](./dev-docs/components/)
- [Third-party libraries](./dev-docs/third-party-libraries.md)

---

## Project Overview

Lumen is a **Wayland compositor that streams the desktop to browsers via WebRTC**. It is a Rust workspace with 5 crates plus a main binary. The browser client is vanilla JavaScript in `web/`.

---

## Crate Map

| Crate              | Path                       | Role                                               |
| ------------------ | -------------------------- | -------------------------------------------------- |
| `lumen`            | `src/`                     | Orchestration only — no business logic             |
| `lumen-compositor` | `crates/lumen-compositor/` | Wayland compositor, frame capture, input injection |
| `lumen-audio`      | `crates/lumen-audio/`      | PipeWire capture, Opus encoding                    |
| `lumen-encode`     | `crates/lumen-encode/`     | H.264 encoding (VA-API + x264 fallback)            |
| `lumen-webrtc`     | `crates/lumen-webrtc/`     | WebRTC sessions, SDP/ICE, RTP packetization        |
| `lumen-web`        | `crates/lumen-web/`        | Axum HTTP server, WebSocket signaling              |

Crates are **intentionally decoupled** — no crate depends on another crate in this workspace. Only the main binary wires them together.

---

## Concurrency Model

- **Compositor** → dedicated `std::thread` (blocking `calloop` loop). Never await inside it.
- **Encoder, Audio** → `tokio::task::spawn_blocking`. CPU-bound, must not run on the async runtime.
- **Everything else** → `tokio::spawn` async tasks.
- Bridge between async world and compositor: `calloop::channel`. Do not use `tokio::sync` channels to talk to the compositor.

See [architecture threading diagram](./dev-docs/architecture.md#threading-and-execution-model).

---

## Inter-Task Communication

- **`tokio::broadcast`** — one-to-many: `CapturedFrame`, `EncodedFrame`, `CursorEvent`, `ClipboardEvent`.
- **`tokio::mpsc`** — one-to-one: `InputEvent` to compositor, `OpusPacket` from audio, resize requests.
- **`Arc<AtomicUsize>`** — lock-free peer count; encoder skips encoding when zero.
- **`Arc<AtomicBool>`** — lock-free keyframe flag; web layer sets, encoder clears.
- Prefer atomic flags over channels for hot-path signals.

See [channel wiring diagram](./dev-docs/components/main-application.md#channel-wiring).

---

## Error Handling

- Library crates: `thiserror` for typed errors, returned as `Result<T, CrateError>`.
- Main binary: `anyhow` for context-enriched results.
- Don't use `unwrap()` in library code; prefer `?` with context.

---

## Rendering Paths

There are two paths — don't conflate them:

- **GPU path** (`render_node = Some(path)`): `GlesRenderer` → DMA-BUF output → VA-API encoder (zero-copy).
- **CPU path** (`render_node = None`): `PixmanRenderer` → RGBA `Vec<u8>` → x264 encoder.

When modifying frame handling code, verify the change works for **both** paths. See [rendering paths](./dev-docs/architecture.md#rendering-paths).

---

## Encoder

- `VideoEncoder` is a trait in `lumen-encode`. Both backends implement it.
- `create_encoder()` is the factory — it probes VA-API, falls back to x264 automatically.
- Output is always **Annex-B H.264** with 4-byte start codes (`0x00 0x00 0x00 0x01`).
- The encoder loop skips encoding when `peer_count == 0`.
- Keyframes are forced on new peer connections and after resize.

See [lumen-encode](./dev-docs/components/lumen-encode.md).

---

## WebRTC / Signaling

- WebRTC stack: `str0m` (pure Rust, sync). Wrap in async tasks; use non-blocking UDP sockets.
- Each peer session has a dedicated `tokio::spawn` drive task calling `session.drive()` in a loop.
- Input events from the browser arrive via data channel JSON, collected via `drain_input_events()`.
- State replay on connect: new data channels receive the last cursor + clipboard JSON immediately.

See [lumen-webrtc](./dev-docs/components/lumen-webrtc.md) and [lumen-web signaling flow](./dev-docs/components/lumen-web.md#signaling-flow).

---

## Input Events

- Keyboard scancodes from the browser are **Linux evdev scancodes**. The compositor adds **+8** to convert to XKB keycodes before dispatching to Smithay.
- Mouse button codes are Linux `BTN_*` values (BTN_LEFT=272, BTN_RIGHT=273, BTN_MIDDLE=274).
- `InputEvent` enum is defined in `lumen-compositor`. Do not define parallel input types elsewhere.

---

## Wayland / Smithay

- All Wayland protocol state lives in `AppState` inside `lumen-compositor`.
- Protocol handlers use Smithay `delegate_*` macros. Follow existing patterns when adding new protocol support.
- Smithay is pinned to a git commit — check `Cargo.toml` before assuming a feature is available.

---

## Adding a New Feature

1. Identify which crate owns the concern.
2. If a new channel is needed, define it in `main.rs` and thread it through `Config` structs — don't use global state.
3. If a new broadcast consumer is needed, call `receiver()` before the compositor thread starts to avoid missing early frames.
4. New Wayland protocols: add delegate impls in `lumen-compositor/src/handlers.rs`, register in `AppState`.
5. New encoder capabilities: implement on both `SoftwareEncoder` and `VaapiEncoder`, extend the `VideoEncoder` trait if needed.

---

## Browser Client

- Vanilla JavaScript, no build step. Files in `web/` are served directly by the Axum static file server.
- `LumenClient` (`lumen-client.js`) owns all WebRTC logic. `LumenUI` (`lumen-ui.js`) owns DOM interaction.
- Data channel messages are JSON. Match the schema in [lumen-webrtc data channel docs](./dev-docs/components/lumen-webrtc.md#data-channel-messages).
- Key mapping table (`KEY_MAP`) maps DOM key names to Linux evdev scancodes — extend it for new keys.

---

## Build & Test

```bash
cargo build --release        # full build
cargo check                  # fast type-check only
cargo test                   # run tests
RUST_LOG=lumen=debug cargo run --release   # run with debug logging
```

Prerequisites: `libx264`, `libva`, `libdrm`, `libopus`, `libwayland-dev`, PipeWire.

---

## Style

- No `unwrap()` in library crates.
- No blocking calls on the Tokio async runtime — use `spawn_blocking` for anything CPU-bound or that calls blocking I/O.
- Prefer `Bytes` (zero-copy) over `Vec<u8>` for data that crosses task boundaries (frames, packets).
- Document public API items with `///` doc comments.
- Keep `main.rs` as thin orchestration — no business logic there.
