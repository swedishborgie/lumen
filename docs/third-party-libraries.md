# Third-Party Libraries

This document lists all significant external dependencies used by Lumen, what they are used for, and where to find their documentation.

## Workspace-Wide Dependencies

These crates are available to all crates in the workspace.

| Crate | Version | Purpose | Documentation |
|-------|---------|---------|---------------|
| [`tokio`](https://crates.io/crates/tokio) | 1.x | Async runtime; provides `spawn`, `spawn_blocking`, broadcast/mpsc channels, timers, and async I/O | [docs.rs/tokio](https://docs.rs/tokio) |
| [`tracing`](https://crates.io/crates/tracing) | 0.1 | Structured, leveled logging and telemetry instrumentation (macros: `info!`, `debug!`, `error!`, etc.) | [docs.rs/tracing](https://docs.rs/tracing) |
| [`tracing-subscriber`](https://crates.io/crates/tracing-subscriber) | 0.3 | Tracing backend; formats and emits log output; configures per-crate log levels via `RUST_LOG` | [docs.rs/tracing-subscriber](https://docs.rs/tracing-subscriber) |
| [`serde`](https://crates.io/crates/serde) | 1.x | Serialization/deserialization framework; enables `#[derive(Serialize, Deserialize)]` | [serde.rs](https://serde.rs) |
| [`serde_json`](https://crates.io/crates/serde_json) | 1.x | JSON codec built on `serde`; used for WebSocket signaling and data channel messages | [docs.rs/serde_json](https://docs.rs/serde_json) |
| [`thiserror`](https://crates.io/crates/thiserror) | 2.x | Derive macro for `std::error::Error`; used to define crate-specific error types | [docs.rs/thiserror](https://docs.rs/thiserror) |
| [`anyhow`](https://crates.io/crates/anyhow) | 1.x | Flexible `Result` type with context chaining; used in application-level error handling | [docs.rs/anyhow](https://docs.rs/anyhow) |
| [`base64`](https://crates.io/crates/base64) | 0.22 | Base64 encoding; used to encode cursor RGBA images for data channel transmission | [docs.rs/base64](https://docs.rs/base64) |
| [`bytes`](https://crates.io/crates/bytes) | 1.x | Zero-copy byte buffer (`Bytes`, `BytesMut`); used for frame data, encoded packets, and cursor images | [docs.rs/bytes](https://docs.rs/bytes) |

## lumen-compositor

| Crate | Version | Purpose | Documentation |
|-------|---------|---------|---------------|
| [`smithay`](https://crates.io/crates/smithay) | git | Comprehensive Wayland compositor library; provides protocol handlers, renderers (GLES, Pixman), input seat management, space/window management, and DMA-BUF support | [Smithay Book](https://smithay.github.io/smithay/) / [docs.rs/smithay](https://docs.rs/smithay) |
| [`wayland-server`](https://crates.io/crates/wayland-server) | 0.31 | Low-level Wayland server protocol bindings | [docs.rs/wayland-server](https://docs.rs/wayland-server) |
| [`wayland-protocols`](https://crates.io/crates/wayland-protocols) | 0.32 | Standard Wayland extension protocols (xdg-shell, xdg-decoration, linux-dmabuf, etc.) | [docs.rs/wayland-protocols](https://docs.rs/wayland-protocols) |
| [`wayland-protocols-wlr`](https://crates.io/crates/wayland-protocols-wlr) | 0.3 | Wlroots extension protocols (layer-shell, screencopy, etc.) | [docs.rs/wayland-protocols-wlr](https://docs.rs/wayland-protocols-wlr) |
| [`calloop`](https://crates.io/crates/calloop) | 0.14 | Callback-based event loop used for the compositor's main loop; handles I/O sources, timers, and inter-thread channels | [docs.rs/calloop](https://docs.rs/calloop) |
| [`calloop-wayland-source`](https://crates.io/crates/calloop-wayland-source) | 0.4 | Integrates the Wayland server socket into the calloop event loop | [docs.rs/calloop-wayland-source](https://docs.rs/calloop-wayland-source) |
| [`gbm`](https://crates.io/crates/gbm) | 0.18 | Safe Rust bindings to `libgbm` (Generic Buffer Management); used to allocate GPU-backed DMA-BUF buffers | [docs.rs/gbm](https://docs.rs/gbm) |
| [`drm`](https://crates.io/crates/drm) | 0.14 | Safe Rust bindings to the Linux DRM (Direct Rendering Manager) subsystem; used to open and manage the GPU render node | [docs.rs/drm](https://docs.rs/drm) |
| [`drm-sys`](https://crates.io/crates/drm-sys) | 0.8 | Low-level FFI bindings to DRM kernel interfaces | [docs.rs/drm-sys](https://docs.rs/drm-sys) |
| [`xkbcommon`](https://crates.io/crates/xkbcommon) | 0.7 | Rust bindings to `libxkbcommon`; handles keyboard layout processing and keycode/keysym translation | [docs.rs/xkbcommon](https://docs.rs/xkbcommon) |
| [`rustix`](https://crates.io/crates/rustix) | 1.x | Safe, low-level system call wrappers; used for file descriptor operations and OS-level utilities | [docs.rs/rustix](https://docs.rs/rustix) |

## lumen-audio

| Crate | Version | Purpose | Documentation |
|-------|---------|---------|---------------|
| [`libpulse-binding`](https://crates.io/crates/libpulse-binding) | 2.28 | Safe Rust bindings to the PulseAudio client library (`libpulse`); provides the main PulseAudio API | [docs.rs/libpulse-binding](https://docs.rs/libpulse-binding) |
| [`libpulse-simple-binding`](https://crates.io/crates/libpulse-simple-binding) | 2.28 | Bindings to the PulseAudio Simple API (`libpulse-simple`); used for straightforward synchronous PCM capture | [docs.rs/libpulse-simple-binding](https://docs.rs/libpulse-simple-binding) |
| [`opus`](https://crates.io/crates/opus) | 0.3 | Rust bindings to `libopus`; provides the Opus audio encoder and decoder | [docs.rs/opus](https://docs.rs/opus) |

## lumen-encode

| Crate | Version | Purpose | Documentation |
|-------|---------|---------|---------------|
| [`ffmpeg-next`](https://crates.io/crates/ffmpeg-next) | 7.x | High-level Rust wrapper around the FFmpeg libraries; used for the VA-API hardware encoding pipeline (filter graph, codec context, frame allocation) | [docs.rs/ffmpeg-next](https://docs.rs/ffmpeg-next) |
| [`ffmpeg-sys-next`](https://crates.io/crates/ffmpeg-sys-next) | 7.x | Low-level FFI bindings to all FFmpeg C libraries (`libavcodec`, `libavfilter`, `libavutil`, etc.) | [docs.rs/ffmpeg-sys-next](https://docs.rs/ffmpeg-sys-next) |
| [`x264-sys`](https://crates.io/crates/x264-sys) | 0.2 | Raw FFI bindings to `libx264`; used by the software encoder backend for H.264 encoding | [docs.rs/x264-sys](https://docs.rs/x264-sys) |
| [`yuv`](https://crates.io/crates/yuv) | 0.8 | YUV color space conversion utilities; used to convert RGBA frames to I420 for the x264 software encoder | [docs.rs/yuv](https://docs.rs/yuv) |
| [`rayon`](https://crates.io/crates/rayon) | 1.x | Data parallelism library; available for potential parallel YUV conversion or other CPU-bound encoding tasks | [docs.rs/rayon](https://docs.rs/rayon) |
| [`libc`](https://crates.io/crates/libc) | 0.2 | C type definitions and standard library bindings; used in FFI code for interoperability with native libraries | [docs.rs/libc](https://docs.rs/libc) |

## lumen-webrtc

| Crate | Version | Purpose | Documentation |
|-------|---------|---------|---------------|
| [`str0m`](https://crates.io/crates/str0m) | 0.16 | Pure-Rust WebRTC stack; handles SDP parsing/generation, ICE agent, DTLS handshake, SRTP key derivation, and RTP packetization | [docs.rs/str0m](https://docs.rs/str0m) / [GitHub](https://github.com/algesten/str0m) |
| [`uuid`](https://crates.io/crates/uuid) | 1.x | UUID generation (v4); used to create unique `SessionId`s for each WebRTC peer | [docs.rs/uuid](https://docs.rs/uuid) |

## lumen-web

| Crate | Version | Purpose | Documentation |
|-------|---------|---------|---------------|
| [`axum`](https://crates.io/crates/axum) | 0.8 | Ergonomic, async HTTP framework built on Tokio and `hyper`; handles HTTP routing, WebSocket upgrades, and request/response handling | [docs.rs/axum](https://docs.rs/axum) |
| [`tower-http`](https://crates.io/crates/tower-http) | 0.6 | Tower middleware for HTTP services; provides static file serving (`ServeDir`), CORS headers, and request tracing | [docs.rs/tower-http](https://docs.rs/tower-http) |
| [`pam`](https://crates.io/crates/pam) | 0.7 | Safe Rust bindings to the system PAM (Pluggable Authentication Modules) library; used to validate credentials in Basic auth mode | [docs.rs/pam](https://docs.rs/pam) |
| [`openidconnect`](https://crates.io/crates/openidconnect) | 3.x | Full OpenID Connect client: OIDC discovery, authorization URL construction, PKCE, authorization code exchange, and ID token signature validation via JWKS | [docs.rs/openidconnect](https://docs.rs/openidconnect) |
| [`cookie`](https://crates.io/crates/cookie) | 0.18 | HTTP cookie parsing and serialization (`Set-Cookie` / `Cookie` headers); used to manage the `lumen_session` cookie in OAuth2 mode | [docs.rs/cookie](https://docs.rs/cookie) |
| [`uuid`](https://crates.io/crates/uuid) | 1.x | UUID generation (v4); used to create unique session tokens for the OAuth2 in-memory session store | [docs.rs/uuid](https://docs.rs/uuid) |

## lumen-turn

| Crate | Version | Purpose | Documentation |
|-------|---------|---------|---------------|
| [`turn`](https://crates.io/crates/turn) | 0.17 | Pure-Rust TURN/STUN server library; provides the relay server, auth handler interface, and relay address generator | [docs.rs/turn](https://docs.rs/turn) |
| [`webrtc-util`](https://crates.io/crates/webrtc-util) | 0.17 | WebRTC utility types; provides the virtual network (`vnet`) abstraction used by the TURN relay address generator | [docs.rs/webrtc-util](https://docs.rs/webrtc-util) |

## lumen-gamepad

| Crate | Version | Purpose | Documentation |
|-------|---------|---------|---------------|
| [`evdev`](https://crates.io/crates/evdev) | 0.12 | Safe Rust bindings to the Linux `evdev` and `uinput` kernel interfaces; used to create virtual gamepad input devices and emit button/axis events | [docs.rs/evdev](https://docs.rs/evdev) |

## Main Binary (`lumen`)

| Crate | Version | Purpose | Documentation |
|-------|---------|---------|---------------|
| [`clap`](https://crates.io/crates/clap) | 4.x | CLI argument parsing; provides `--flag` arguments and `LUMEN_*` environment variable bindings with the `env` feature | [docs.rs/clap](https://docs.rs/clap) |

## Key Technology Notes

### Why Smithay?
Smithay is the primary Rust-native Wayland compositor library. It provides protocol handlers, rendering backends (GLES2, Pixman), seat/input management, and DMA-BUF integration — everything needed to build a full compositor without writing Wayland protocol code from scratch.

### Why str0m?
str0m is a pure-Rust, sans-I/O WebRTC implementation. It does not dictate the I/O model, making it straightforward to integrate into Tokio. It handles the full WebRTC stack (ICE, DTLS, SRTP, RTP) without depending on the system's `libwebrtc`.

### Why FFmpeg for VA-API?
The VA-API hardware encoding path uses FFmpeg's filter graph (`hwmap` + `scale_vaapi`) because it provides a well-tested, zero-copy DMA-BUF → VA-API pipeline that handles pixel format conversion (ARGB → NV12) entirely on the GPU, avoiding the complexity of building this pipeline manually via raw VA-API calls.

### Why x264 for software fallback?
libx264's `ultrafast` + `zerolatency` preset combination offers the best real-time encoding latency of any open-source H.264 software encoder, at the cost of compression efficiency. This is the correct tradeoff for interactive streaming.

### Why Opus?
Opus is the standard codec for WebRTC audio. It offers excellent quality at low bitrates, minimal algorithmic latency in `LowDelay` mode, and native support in all WebRTC-capable browsers.
