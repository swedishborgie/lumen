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
| **System audio** | PipeWire virtual sink capture, Opus-encoded, synchronized with video |
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
│   ├── lumen-audio/        # PipeWire capture, Opus encoding
│   ├── lumen-encode/       # H.264 encoding (VA-API + x264)
│   ├── lumen-webrtc/       # WebRTC session management
│   ├── lumen-web/          # HTTP server, WebSocket signaling
│   ├── lumen-turn/         # Embedded TURN relay server
│   └── lumen-gamepad/      # Virtual uinput gamepad devices
└── web/                    # Browser client (HTML/JS/CSS)
```

## Build Prerequisites

| Dependency | Purpose |
|-----------|---------|
| Rust (latest stable) | Build toolchain |
| `libx264` dev headers | Software H.264 encoding fallback |
| `libva`, `libdrm` dev headers | VA-API hardware encoding |
| `libopus` dev headers | Opus audio codec |
| PipeWire | Audio capture |
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
| `--log-output` | `LUMEN_LOG_OUTPUT` | `stderr` | Log output destination: `stderr` (default), `journald`, or `file:/path/to/log` |
| `--syslog-identifier` | `LUMEN_SYSLOG_IDENTIFIER` | *(binary name)* | Syslog identifier for journald output; set to systemd unit instance name (e.g. `lumen@alice`) |
| `--bind-addr` | `LUMEN_BIND` | `0.0.0.0:8080` | HTTP/WebSocket bind address |
| `--hostname` | `LUMEN_HOSTNAME` | *(OS hostname)* | Hostname shown in the browser tab title and PWA app name |
| `--width` | `LUMEN_WIDTH` | `1920` | Output width in pixels |
| `--height` | `LUMEN_HEIGHT` | `1080` | Output height in pixels |
| `--fps` | `LUMEN_FPS` | `30.0` | Target frames per second |
| `--video-bitrate-kbps` | `LUMEN_VIDEO_BITRATE_KBPS` | `4000` | Video encoder target bitrate (kbps) |
| `--max-bitrate-kbps` | `LUMEN_MAX_BITRATE_KBPS` | *(2× target)* | Peak bitrate cap in kbps for VBR encoding |
| `--audio-bitrate-bps` | `LUMEN_AUDIO_BITRATE_BPS` | `128000` | Opus audio encoder bitrate (bps) |
| `--dri-node` | `LUMEN_DRI_NODE` | *(auto-detect)* | Path to DRI render node (e.g. `/dev/dri/renderD128`) for GPU acceleration |
| `--cuda-device` | `LUMEN_CUDA_DEVICE` | `0` | CUDA device index for NVENC hardware encoding (nvenc feature only); set to empty string to disable NVENC |
| `--inner-display` | `LUMEN_INNER_DISPLAY` | `auto` | Wayland socket of a nested compositor for clipboard bridging; `auto` = scan `$XDG_RUNTIME_DIR`; empty string = disabled |
| `--ice-servers` | `LUMEN_ICE_SERVERS` | `stun:stun.l.google.com:19302` | Comma-separated STUN/TURN server URLs (used only when the embedded TURN server is disabled) |
| `--turn-port` | `LUMEN_TURN_PORT` | `3478` | UDP port for the embedded TURN server; set to `0` to disable |
| `--turn-external-ip` | `LUMEN_TURN_EXTERNAL_IP` | *(auto-detect)* | Public IP advertised as the TURN relay address; falls back to `127.0.0.1` |
| `--turn-username` | `LUMEN_TURN_USERNAME` | *(auto-generated)* | TURN credential username |
| `--turn-password` | `LUMEN_TURN_PASSWORD` | *(auto-generated)* | TURN credential password |
| `--turn-min-port` | `LUMEN_TURN_MIN_PORT` | `50000` | Lowest UDP port in the TURN relay range |
| `--turn-max-port` | `LUMEN_TURN_MAX_PORT` | `50010` | Highest UDP port in the TURN relay range |
| `--auth` | `LUMEN_AUTH` | `basic` | Authentication mode: `none`, `basic` (PAM), `bearer` (preshared token), or `oauth2` (OIDC) |
| `--auth-bearer-token` | `LUMEN_AUTH_BEARER_TOKEN` | *(required for bearer)* | Preshared token for bearer authentication |
| `--auth-oauth2-issuer-url` | `LUMEN_AUTH_OAUTH2_ISSUER_URL` | *(required for oauth2)* | OIDC issuer URL |
| `--auth-oauth2-client-id` | `LUMEN_AUTH_OAUTH2_CLIENT_ID` | *(required for oauth2)* | OAuth2 client ID |
| `--auth-oauth2-client-secret` | `LUMEN_AUTH_OAUTH2_CLIENT_SECRET` | *(required for oauth2)* | OAuth2 client secret |
| `--auth-oauth2-redirect-uri` | `LUMEN_AUTH_OAUTH2_REDIRECT_URI` | *(required for oauth2)* | Full callback URL, e.g. `http://localhost:8080/auth/callback` |
| `--auth-oauth2-subject` | `LUMEN_AUTH_OAUTH2_SUBJECT` | *(required for oauth2)* | Expected `sub` claim in the validated ID token |
| `--desktop` | `LUMEN_DESKTOP` | *(none)* | Named desktop environment preset: `labwc` or `kde`; sets default launch command and required env vars |
| `--launch` | `LUMEN_LAUNCH` | *(none)* | Shell command to launch as a Wayland client once the compositor is ready (passed to `/bin/sh -c`) |
| `--tls-cert` | `LUMEN_TLS_CERT` | *(none)* | Path to a PEM-encoded TLS certificate chain (enables HTTPS when combined with `--tls-key`) |
| `--tls-key` | `LUMEN_TLS_KEY` | *(none)* | Path to a PEM-encoded TLS private key (must be provided together with `--tls-cert`) |

### Logging

Lumen uses `tracing` with `RUST_LOG`-style filtering:

```bash
RUST_LOG=lumen=info,lumen_compositor=debug cargo run --release
```
