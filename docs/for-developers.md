---
title: For Developers
layout: default
nav_order: 4
description: "Set up a build environment to develop and contribute to Lumen."
---

# For Developers
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

This page covers everything you need to build Lumen from source, run it locally, and get oriented enough to start contributing.

---

## Build Prerequisites

Lumen links against several native libraries. Install the development headers for your distribution before running `cargo build`.

### Fedora / RHEL

```bash
sudo dnf install \
    rust cargo \
    gcc gcc-c++ \
    cmake \
    nasm \
    pkg-config \
    clang \
    clang-devel \
    llvm \
    wayland-devel \
    libxkbcommon-devel \
    xkeyboard-config \
    pixman-devel \
    mesa-libEGL-devel \
    mesa-libGLES-devel \
    mesa-libgbm-devel \
    libdrm-devel \
    libinput-devel \
    libevdev-devel \
    systemd-devel \
    pulseaudio-libs-devel \
    pipewire-devel \
    opus-devel \
    x264-devel \
    libva-devel \
    ffmpeg-devel \
    pam-devel \
    openssl-devel
```

You'll need the [RPM Fusion enabled](https://rpmfusion.org/Configuration).
You will also need to [Switch to full ffmpeg](https://rpmfusion.org/Howto/Multimedia).

### Ubuntu / Debian

```bash
sudo apt install \
    rustup \
    build-essential \
    cmake \
    nasm \
    pkg-config \
    git \
    clang \
    libclang-dev \
    llvm \
    libwayland-dev \
    libxkbcommon-dev \
    libpixman-1-dev \
    libegl-dev \
    libgles2-mesa-dev \
    libgbm-dev \
    libdrm-dev \
    libinput-dev \
    libevdev-dev \
    libudev-dev \
    libpulse-dev \
    libpipewire-0.3-dev \
    libspa-0.2-dev \
    libopus-dev \
    libx264-dev \
    libva-dev \
    libavcodec-dev \
    libavformat-dev \
    libavutil-dev \
    libavfilter-dev \
    libavdevice-dev \
    libswscale-dev \
    libswresample-dev \
    libpam0g-dev \
    libssl-dev
```

Then install Rust via rustup if not already installed:

```bash
rustup default stable
```

---

## Clone and Build

```bash
git clone https://github.com/your-org/lumen.git
cd lumen
cargo build --release
```

The first build downloads and compiles all Rust dependencies along with native C/C++ libraries (FFmpeg bindings, Smithay, etc.) and will take several minutes. Subsequent builds are incremental.

For a faster iteration cycle during development:

```bash
cargo build          # debug build (faster compile, slower runtime)
cargo check          # type-check only — fastest feedback loop
```

---

## Run

```bash
cargo run --release
```

Then open `http://localhost:8080` in a browser and click **Connect**.

### Launch a Wayland client on startup

Use `--launch` to start a compositor or application once Lumen is ready:

```bash
cargo run --release -- --launch labwc
cargo run --release -- --launch "sway --config /path/to/sway.conf"
```

### Logging

Lumen uses `tracing` with `RUST_LOG`-style filtering:

```bash
# General info
RUST_LOG=info cargo run --release

# Debug a specific crate
RUST_LOG=lumen_compositor=debug cargo run --release

# Multiple targets
RUST_LOG=lumen=info,lumen_compositor=debug,lumen_webrtc=trace cargo run --release
```

---

## Tests

```bash
cargo test
```

---

## Workspace Structure

Lumen is a Cargo workspace. All business logic lives in the crates — the main binary in `src/` is thin orchestration only.

| Crate | Path | Role |
|-------|------|------|
| `lumen` (binary) | `src/` | Wires crates together; no business logic |
| `lumen-compositor` | `crates/lumen-compositor/` | Wayland compositor, frame capture, input injection |
| `lumen-audio` | `crates/lumen-audio/` | PulseAudio capture, Opus encoding |
| `lumen-encode` | `crates/lumen-encode/` | H.264 encoding (VA-API + x264 fallback) |
| `lumen-webrtc` | `crates/lumen-webrtc/` | WebRTC sessions, ICE/SDP, RTP packetization |
| `lumen-web` | `crates/lumen-web/` | Axum HTTP server, WebSocket signaling |
| `lumen-turn` | `crates/lumen-turn/` | Embedded TURN/STUN relay server |
| `lumen-gamepad` | `crates/lumen-gamepad/` | Virtual uinput gamepad devices |
| *(browser client)* | `web/` | Vanilla JavaScript; served by `lumen-web` |

**The crates are intentionally decoupled** — no crate depends on another crate in this workspace. Only `main.rs` wires them together by threading channels and `Arc` values through each crate's `Config` struct.

---

## Key Concepts for Contributors

### Concurrency Model

| Component | Execution model | Reason |
|-----------|----------------|--------|
| Compositor | Dedicated `std::thread` (blocking `calloop` loop) | Must never be blocked by async scheduling |
| Encoder / Audio | `tokio::task::spawn_blocking` | CPU-bound; must not block the async thread pool |
| Everything else | `tokio::spawn` (async) | Network I/O, signaling, fan-out |

**Never `await` inside the compositor thread.** Use `calloop::channel` to send events to it from async tasks — not `tokio::sync` channels.

### Channel Wiring

All channels are created in `main.rs` and threaded through each crate's `Config` struct. There is no global state. When adding a new channel:

1. Define it in `main.rs`.
2. Add it to the relevant `Config` struct(s).
3. If it's a `tokio::broadcast` receiver, call `receiver()` **before** the compositor thread starts to avoid missing early frames.

### Rendering Paths

There are two paths — changes to frame-handling code must work for both:

- **GPU path** (`render_node = Some(path)`): `GlesRenderer` → DMA-BUF → VA-API encoder (zero-copy)
- **CPU path** (`render_node = None`): `PixmanRenderer` → RGBA `Vec<u8>` → x264 encoder

See the [Architecture page](../architecture#rendering-paths) for a full description.

### Error Handling

- **Library crates**: `thiserror` for typed errors returned as `Result<T, CrateError>`.
- **Main binary**: `anyhow` for context-enriched results.
- No `unwrap()` in library code — propagate with `?` and add context where helpful.

### Browser Client

The browser client (`web/`) is vanilla JavaScript with no build step. Files are served directly by `lumen-web`. Key files:

| File | Role |
|------|------|
| `web/lumen-client.mjs` | All WebRTC logic (`LumenClient` class) |
| `web/lumen-ui.mjs` | DOM interaction (`LumenUI` class) |
| `web/index.html` | Entry point |

Data channel messages between the browser and server are JSON. Input events from the browser use Linux evdev scancodes for keyboard (the compositor adds +8 to convert to XKB keycodes) and `BTN_*` values for mouse buttons.

---

## Configuration Reference

All options can be set via CLI flags or environment variables. Run `lumen --help` for the full list, or see the table below for the most common options.

| Flag | Env | Default | Description |
|------|-----|---------|-------------|
| `--bind-addr` | `LUMEN_BIND` | `0.0.0.0:8080` | HTTP/WebSocket bind address |
| `--width` | `LUMEN_WIDTH` | `1920` | Output width in pixels |
| `--height` | `LUMEN_HEIGHT` | `1080` | Output height in pixels |
| `--fps` | `LUMEN_FPS` | `30.0` | Target frames per second |
| `--video-bitrate-kbps` | `LUMEN_VIDEO_BITRATE_KBPS` | `4000` | Video encoder target bitrate (kbps) |
| `--dri-node` | `LUMEN_DRI_NODE` | *(auto-detect)* | DRI render node for VA-API (e.g. `/dev/dri/renderD128`) |
| `--launch` | `LUMEN_LAUNCH` | | Shell command to launch as a Wayland client on startup |
| `--auth` | `LUMEN_AUTH` | `none` | Auth mode: `none`, `basic`, `bearer`, `oauth2` |
| `--turn-port` | `LUMEN_TURN_PORT` | `3478` | Embedded TURN server UDP port (`0` to disable) |
| `--tls-cert` | `LUMEN_TLS_CERT` | | Path to PEM TLS certificate (enables HTTPS with `--tls-key`) |
| `--tls-key` | `LUMEN_TLS_KEY` | | Path to PEM TLS private key |
