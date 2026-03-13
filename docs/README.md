# Lumen Documentation

This directory contains technical documentation for the Lumen project — a Wayland-based compositor that streams desktop sessions to web browsers over WebRTC.

## Contents

| Document | Description |
|----------|-------------|
| [Overview](./overview.md) | High-level description of Lumen, its purpose, features, and how to build and configure it |
| [Architecture](./architecture.md) | System architecture, component interaction diagrams, data flows, threading model, and protocols |
| [Third-Party Libraries](./third-party-libraries.md) | All significant external dependencies, their purpose, and links to documentation |

### Component Design

Detailed design documents for each crate and the main application:

| Document | Crate / Module |
|----------|----------------|
| [Main Application](./components/main-application.md) | `src/main.rs` — orchestration, task spawning, channel wiring |
| [lumen-compositor](./components/lumen-compositor.md) | `crates/lumen-compositor` — Wayland compositor, rendering, frame capture, input injection |
| [lumen-audio](./components/lumen-audio.md) | `crates/lumen-audio` — PulseAudio capture and Opus encoding |
| [lumen-encode](./components/lumen-encode.md) | `crates/lumen-encode` — H.264 video encoding (hardware VA-API and software x264) |
| [lumen-webrtc](./components/lumen-webrtc.md) | `crates/lumen-webrtc` — WebRTC session management, SDP/ICE, RTP packetization |
| [lumen-web](./components/lumen-web.md) | `crates/lumen-web` — HTTP server, WebSocket signaling, browser client |
