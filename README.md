# Lumen

Lumen is a Wayland-based compositor designed to stream desktop applications and desktop environments to a browser using WebRTC. It provides a high-performance, low-latency remote desktop experience by combining modern Wayland protocols with hardware-accelerated video encoding and robust WebRTC delivery.

## Key Features

- **Wayland-Native**: Built on top of [Smithay](https://github.com/Smithay/smithay), providing a modern, secure foundation for window management and rendering.
- **WebRTC Streaming**: Low-latency video and audio streaming directly to any modern web browser using the [str0m](https://github.com/algesten/str0m) WebRTC stack.
- **Hardware Acceleration**: Support for VA-API (Intel/AMD) for zero-copy hardware-accelerated H.264 encoding, with a software x264 fallback.
- **System Audio**: Captures system audio via PulseAudio and encodes it to Opus for high-quality, low-latency sound.
- **Interactive**: Forwards keyboard and mouse events from the browser back to the Wayland session.
- **Standalone**: Includes a built-in web server and signaling handler, making it easy to deploy as a single binary.

## Project Structure

Lumen is organized as a Rust workspace with several specialized crates:

- **`lumen-compositor`**: The core Wayland compositor logic using Smithay. Handles window management, rendering, and frame capture.
- **`lumen-webrtc`**: Manages WebRTC sessions, ICE negotiation (via str0m), and RTP packetization for H.264 and Opus.
- **`lumen-encode`**: Video encoding abstraction layer. Supports VA-API and software (x264) backends.
- **`lumen-audio`**: Captures system audio from PulseAudio monitor sources and encodes it using Opus.
- **`lumen-web`**: An Axum-based web server that serves the frontend client and handles WebSocket signaling.
- **`web/`**: The frontend browser client built with vanilla JavaScript and Web APIs.

## Getting Started

### Prerequisites

- Rust (latest stable)
- PulseAudio / PipeWire (with PulseAudio compatibility)
- VA-API compatible drivers (optional, for hardware acceleration)
- `libx264`, `libva`, `libopus` development headers

### Build

```bash
cargo build --release
```

### Run

You can run Lumen with default settings:

```bash
cargo run --release
```

Then, open your browser and navigate to `http://localhost:8080`.

### Configuration

Lumen can be configured via command-line arguments or environment variables:

| Argument | Environment Variable | Default | Description |
|----------|----------------------|---------|-------------|
| `--bind-addr` | `LUMEN_BIND` | `0.0.0.0:8080` | Address to bind the web server to |
| `--width` | `LUMEN_WIDTH` | `1920` | Output width |
| `--height` | `LUMEN_HEIGHT` | `1080` | Output height |
| `--fps` | `LUMEN_FPS` | `30.0` | Target frames per second |
| `--video-bitrate-kbps` | `LUMEN_VIDEO_BITRATE_KBPS` | `4000` | Video bitrate in kbps |
| `--audio-device` | `LUMEN_AUDIO_DEVICE` | | PulseAudio device to capture (default: monitor) |
| `--dri-node` | `LUMEN_DRI_NODE` | | Path to the DRI device node (e.g., `/dev/dri/renderD128`) |
| `--ice-servers` | `LUMEN_ICE_SERVERS` | `stun:stun.l.google.com:19302` | Comma-separated list of ICE/STUN servers |
| `--static-dir` | `LUMEN_STATIC_DIR` | `./web` | Directory containing the web client assets |

## Architecture

1.  **Compositor**: Renders the desktop/applications. Frames are captured and sent to the Encoder.
2.  **Audio**: Captures audio samples from PulseAudio and encodes them into Opus packets.
3.  **Encoder**: Takes raw frames and produces H.264 bitstream (using VA-API if available).
4.  **Signaling**: The browser connects to the Web Server via WebSocket to exchange SDP offers/answers and ICE candidates.
5.  **WebRTC**: Once the connection is established, H.264 and Opus packets are sent over SRTP. Input events are sent back from the browser via WebRTC Data Channels.
6.  **Input**: The Compositor receives input events and injects them into the virtual keyboard/pointer devices.
