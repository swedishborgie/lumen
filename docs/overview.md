# Lumen — High-Level Overview

## What Is Lumen?

Lumen is a **Wayland compositor that streams the desktop to a web browser in real time using WebRTC**. It allows users to run a full graphical desktop session (or individual Wayland applications) on a server and interact with that session from any modern browser — no client software required.

The result is a low-latency, hardware-accelerated remote desktop experience delivered over a standard web page.

## Problem Statement

Traditional remote desktop solutions (VNC, RDP, X forwarding) either require dedicated clients, rely on legacy X11, or suffer from high latency and poor video quality for graphical workloads. Cloud gaming and browser-based streaming have shown that WebRTC is an excellent transport for interactive video, but tooling for compositing and streaming a full desktop over WebRTC has been sparse.

Lumen bridges this gap by combining:

- A modern **Wayland compositor** (via [Smithay](https://github.com/Smithay/smithay)) for native window management and rendering
- **Hardware-accelerated H.264 encoding** (VA-API on Intel/AMD GPUs) with a software fallback
- **WebRTC delivery** (via [str0m](https://github.com/algesten/str0m)) directly to the browser
- **Full interactivity** — keyboard, mouse, clipboard, and cursor synchronization

## User Experience

1. Start Lumen on a machine with a Wayland-capable display server or in a headless environment.
2. Open a browser and navigate to the configured address (default: `http://localhost:8080`).
3. Click **Connect**. The browser exchanges WebRTC signaling over a WebSocket, then begins receiving live H.264 video and Opus audio via SRTP.
4. Interact normally — keyboard input, mouse clicks, scrolling, and clipboard operations all round-trip back to the compositor.
5. The browser cursor is replaced by the server-side cursor image, keeping the experience native-feeling.
6. Click **Disconnect** or close the tab to end the session.

## Key Features

| Feature | Description |
|---------|-------------|
| **Wayland-native** | Built on Smithay; no X11 dependency |
| **WebRTC streaming** | H.264 video + Opus audio over SRTP to any WebRTC-capable browser |
| **Hardware acceleration** | Zero-copy VA-API encoding (Intel/AMD) via FFmpeg filter graph |
| **Software fallback** | Automatic fallback to x264 when GPU acceleration is unavailable |
| **System audio** | PulseAudio monitor source capture, Opus-encoded, synchronized with video |
| **Full input support** | Keyboard (evdev scancodes), mouse (buttons + scroll), clipboard (bidirectional) |
| **Dynamic resize** | Browser can request a new output resolution at runtime |
| **Multi-client** | Multiple browsers can connect simultaneously; frames are encoded once and fanned out |
| **Cursor sync** | Server cursor image and position sent via WebRTC data channel |
| **State replay** | New connections immediately receive the current cursor and clipboard state |
| **Single binary** | Built-in HTTP server and signaling — no external proxy needed |

## Project Structure

```
lumen/
├── src/                    # Main binary — orchestration
├── crates/
│   ├── lumen-compositor/   # Wayland compositor, frame capture, input injection
│   ├── lumen-audio/        # PulseAudio capture, Opus encoding
│   ├── lumen-encode/       # H.264 encoding (VA-API + x264)
│   ├── lumen-webrtc/       # WebRTC session management
│   └── lumen-web/          # HTTP server, WebSocket signaling
└── web/                    # Browser client (HTML/JS/CSS)
```

## Build Prerequisites

| Dependency | Purpose |
|-----------|---------|
| Rust (latest stable) | Build toolchain |
| `libx264` dev headers | Software H.264 encoding fallback |
| `libva`, `libdrm` dev headers | VA-API hardware encoding |
| `libopus` dev headers | Opus audio codec |
| PulseAudio or PipeWire (with PulseAudio compatibility layer) | Audio capture |
| Wayland development libraries (`libwayland-dev`) | Compositor foundation |

### Build

```bash
cargo build --release
```

### Run

```bash
cargo run --release
```

Then open `http://localhost:8080` in a browser.

## Configuration

All options can be set via CLI flags or environment variables.

| CLI Flag | Environment Variable | Default | Description |
|----------|----------------------|---------|-------------|
| `--bind-addr` | `LUMEN_BIND` | `0.0.0.0:8080` | HTTP/WebSocket bind address |
| `--width` | `LUMEN_WIDTH` | `1920` | Output width in pixels |
| `--height` | `LUMEN_HEIGHT` | `1080` | Output height in pixels |
| `--fps` | `LUMEN_FPS` | `30.0` | Target frames per second |
| `--video-bitrate-kbps` | `LUMEN_VIDEO_BITRATE_KBPS` | `4000` | Video encoder target bitrate |
| `--audio-device` | `LUMEN_AUDIO_DEVICE` | *(auto)* | PulseAudio source name (defaults to the default monitor source) |
| `--dri-node` | `LUMEN_DRI_NODE` | *(none)* | Path to DRI render node (e.g. `/dev/dri/renderD128`) for GPU acceleration |
| `--ice-servers` | `LUMEN_ICE_SERVERS` | `stun:stun.l.google.com:19302` | Comma-separated STUN/TURN server URLs |
| `--static-dir` | `LUMEN_STATIC_DIR` | `./web` | Directory containing the browser client assets |

### Logging

Lumen uses `tracing` with `RUST_LOG`-style filtering:

```bash
RUST_LOG=lumen=info,lumen_compositor=debug cargo run --release
```
