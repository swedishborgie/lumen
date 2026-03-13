# Lumen — Architecture

This document describes the overall system architecture of Lumen: how its components are structured, how they communicate, and how data flows from the compositor to the browser.

## Crate Dependency Graph

Lumen is organized as a Cargo workspace. The main binary depends on all five specialized crates; the crates themselves are intentionally decoupled from one another.

```mermaid
graph TD
    main["lumen (binary)"]
    comp["lumen-compositor"]
    audio["lumen-audio"]
    enc["lumen-encode"]
    webrtc["lumen-webrtc"]
    web["lumen-web"]

    main --> comp
    main --> audio
    main --> enc
    main --> webrtc
    main --> web
```

## Component Overview

```mermaid
graph LR
    subgraph Server
        Compositor["lumen-compositor\n(Wayland compositor)"]
        Encoder["lumen-encode\n(H.264 encoder)"]
        Audio["lumen-audio\n(PulseAudio + Opus)"]
        WebRTC["lumen-webrtc\n(WebRTC sessions)"]
        Web["lumen-web\n(HTTP + WS signaling)"]
    end

    subgraph Browser
        JS["JavaScript client\n(WebRTC + data channel)"]
    end

    Compositor -->|"CapturedFrame\n(broadcast channel)"| Encoder
    Encoder -->|"EncodedFrame\n(broadcast channel)"| WebRTC
    Audio -->|"OpusPacket\n(mpsc channel)"| WebRTC
    Compositor -->|"CursorEvent\n(broadcast channel)"| Web
    Compositor -->|"ClipboardEvent\n(broadcast channel)"| Web
    WebRTC <-->|"SRTP (RTP/UDP)\nData Channel"| JS
    JS <-->|"WebSocket\n(SDP + ICE)"| Web
    Web -->|"InputEvent\n(mpsc channel)"| Compositor
    Web -->|"Resize\n(mpsc channel)"| Compositor
    Web <-->|"Session control"| WebRTC
```

## Threading and Execution Model

Different parts of the system require different concurrency models. The compositor and encoders are blocking and live on dedicated threads; networking and coordination are async.

```mermaid
graph TD
    subgraph OS Thread
        CT["Compositor\ncalloop event loop\n(blocking)"]
    end

    subgraph Tokio Blocking Tasks
        AT["Audio Capture\nPulseAudio loop\n(spawn_blocking)"]
        ET["Encoder\nH.264 encode loop\n(spawn_blocking)"]
    end

    subgraph Tokio Async Tasks
        IF["Input Forwarding"]
        RC["Resize Coordinator"]
        VF["Video Fan-Out"]
        AF["Audio Fan-Out"]
        CF["Cursor Fan-Out"]
        ClF["Clipboard Fan-Out"]
        WS["Web Server\n(Axum)"]
        DS["Per-Session Drive Tasks"]
    end

    CT <-->|"calloop channel"| IF
    AT -->|"mpsc"| AF
    ET -->|"broadcast"| VF
    VF --> DS
    AF --> DS
    CF --> DS
    ClF --> DS
    WS --> DS
    IF --> CT
    RC --> CT
    RC --> ET
```

## Full Data Flow

### Video Path (Compositor → Browser)

```mermaid
sequenceDiagram
    participant C as Compositor
    participant E as Encoder
    participant VF as Video Fan-Out
    participant W as WebRtcSession
    participant B as Browser

    C->>C: Render frame (GPU or CPU)
    C->>E: CapturedFrame (DMA-BUF or RGBA)
    E->>E: Encode H.264 (VA-API or x264)
    E->>VF: EncodedFrame (Annex-B NAL units)
    VF->>W: push_video(frame)
    W->>W: Packetize into RTP (90 kHz clock)
    W->>B: RTP over SRTP/UDP
    B->>B: Decode + render in <video>
```

### Input Path (Browser → Compositor)

```mermaid
sequenceDiagram
    participant B as Browser
    participant W as WebRtcSession
    participant IF as Input Forwarding Task
    participant C as Compositor

    B->>W: Data channel message (JSON InputEvent)
    W->>W: drain_input_events()
    W->>IF: InputEvent (keyboard/pointer/clipboard)
    IF->>C: InputSender::send(event)
    C->>C: inject_input() → Smithay seat dispatch
    C->>C: Focused Wayland surface receives event
```

### WebRTC Signaling (Browser ↔ Server)

```mermaid
sequenceDiagram
    participant B as Browser
    participant WS as WebSocket (lumen-web)
    participant SM as SessionManager
    participant S as WebRtcSession

    B->>WS: Connect to /ws/signal
    B->>WS: {"type":"offer","sdp":"..."}
    WS->>SM: create_session(offer_sdp)
    SM->>S: new WebRtcSession
    S-->>SM: answer SDP
    SM-->>WS: (session_id, answer_sdp)
    WS-->>B: {"type":"answer","sdp":"...","session_id":"..."}
    loop ICE trickle
        B->>WS: {"type":"candidate","candidate":"..."}
        WS->>S: add_remote_candidate()
    end
    B-->S: DTLS handshake + SRTP setup (via UDP)
    Note over B,S: Media flows directly over SRTP/UDP
```

### Cursor Synchronization

```mermaid
sequenceDiagram
    participant C as Compositor
    participant CF as Cursor Fan-Out Task
    participant W as WebRtcSession
    participant B as Browser

    C->>CF: CursorEvent (Image / Default / Hidden)
    CF->>CF: Serialize to JSON, cache as last_cursor_json
    CF->>W: push_dc_message(cursor_json)
    W->>B: Data channel message

    Note over W,B: On new connection
    W->>W: DC opens
    W->>CF: Request last_cursor_json replay
    CF->>W: push_dc_message(last_cursor_json)
    W->>B: Current cursor state immediately
```

## Communication Channels Summary

| Channel | Type | Producer | Consumer(s) | Payload |
|---------|------|----------|-------------|---------|
| Captured frames | `tokio::broadcast` | Compositor | Encoder | `CapturedFrame` |
| Encoded frames | `tokio::broadcast` | Encoder | Video fan-out | `EncodedFrame` |
| Audio packets | `tokio::mpsc` | Audio capture | Audio fan-out | `OpusPacket` |
| Cursor events | `tokio::broadcast` | Compositor | Cursor fan-out | `CursorEvent` |
| Clipboard events | `tokio::broadcast` | Compositor | Clipboard fan-out | `ClipboardEvent` |
| Input events | `tokio::mpsc` | Web/WebRTC drive tasks | Input forwarding task | `InputEvent` |
| Resize | `tokio::mpsc` | Web server | Resize coordinator | `(u32, u32)` |
| Compositor commands | `calloop::channel` | Async tasks | Compositor thread | `InputEvent`, resize |

## Key Protocols

| Protocol | Layer | Purpose |
|---------|-------|---------|
| **Wayland** | IPC (Unix socket) | Window manager ↔ application communication |
| **WebSocket** | TCP/HTTP upgrade | SDP offer/answer and ICE candidate exchange |
| **ICE** | UDP | NAT traversal and peer connectivity establishment |
| **DTLS** | UDP | Key exchange for SRTP |
| **SRTP** | UDP | Encrypted RTP media transport |
| **RTP** (H.264, RFC 6184) | SRTP | Video frame packetization and delivery |
| **RTP** (Opus, RFC 7587) | SRTP | Audio packet delivery |
| **WebRTC Data Channel** | SCTP over DTLS | Input events, cursor updates, clipboard sync |

## Rendering Paths

Lumen supports two rendering paths depending on whether a DRI render node is configured:

```mermaid
flowchart TD
    Config{"DRI node\nconfigured?"}
    Config -->|Yes| GPU["GPU Path\nGBM + EGL + GlesRenderer\nOutput: DMA-BUF (ARGB8888)"]
    Config -->|No| CPU["CPU Path\nPixmanRenderer\nOutput: RGBA buffer (Vec<u8>)"]

    GPU -->|"DMA-BUF (zero-copy)"| VAAPI["VA-API Encoder\nFFmpeg filter graph\nARGB → NV12 → H.264"]
    CPU -->|"RGBA buffer"| x264["x264 Encoder\nRGBA → I420 → H.264\n(ultrafast/zerolatency)"]

    VAAPI --> Out["EncodedFrame\n(Annex-B H.264)"]
    x264 --> Out
```

The GPU path avoids any CPU memory copy: the compositor renders into a GPU-allocated DMA-BUF, the FFmpeg filter graph maps that buffer directly into the VA-API encoder pipeline, and H.264 NAL units come out the other side.
