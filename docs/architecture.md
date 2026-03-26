---
title: Architecture
layout: default
nav_order: 2
description: "High-level architecture of Lumen: how the browser connects to the server and how the internal pieces fit together."
---

# Architecture
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

## Browser ↔ Server Overview

A browser connects to Lumen using two distinct connections:

1. **WebSocket** (TCP) — used during setup to negotiate the WebRTC connection (signaling).
2. **WebRTC** (UDP) — once connected, all media and input flow over encrypted UDP.

The embedded TURN server acts as a relay so the stream works across NAT without any external infrastructure.

```mermaid
graph LR
    Browser["🌐 Browser"]

    subgraph lumen["Lumen Server"]
        Web["lumen-web\nHTTP · WebSocket"]
        WebRTC["lumen-webrtc\nWebRTC Sessions"]
        TURN["lumen-turn\nTURN Relay"]
    end

    Browser -- "① HTTP\nServes the UI" --> Web
    Browser -- "② WebSocket\nSDP + ICE signaling" --> Web
    Browser -- "③ SRTP/UDP\nH.264 video + Opus audio" --> WebRTC
    Browser -. "④ UDP (if NAT)\nRelayed media" .-> TURN
    WebRTC -. "Relay" .-> TURN
```

---

## Internal Pipeline

Inside the server, data flows in two directions simultaneously — video and audio travel from the server to the browser, while input events travel from the browser back to the compositor.

```mermaid
graph LR
    Apps["Wayland Apps\nlabwc · Firefox · etc."]

    subgraph lumen["Lumen Server"]
        Compositor["lumen-compositor\nWayland Compositor"]
        Encoder["lumen-encode\nH.264 Encoder"]
        Audio["lumen-audio\nVirtual PW Sink\n+ Opus Encoder"]
        WebRTC["lumen-webrtc\nWebRTC Sessions"]
        Web["lumen-web\nHTTP · Signaling"]
    end

    Browser["🌐 Browser"]

    Apps -- "Wayland" --> Compositor
    Apps -- "PipeWire\n(audio routed to\nlumen_capture sink)" --> Audio
    Compositor -- "Raw frames" --> Encoder
    Encoder -- "H.264" --> WebRTC
    Audio -- "Opus" --> WebRTC
    WebRTC -- "SRTP/UDP" --> Browser
    Browser -- "Keyboard · Mouse\nClipboard" --> WebRTC
    WebRTC -- "Input events" --> Compositor
    Browser -- "WebSocket\nSignaling" --> Web
    Web -- "Session control" --> WebRTC
```

---

## How a Connection Is Established

When a browser clicks **Connect**, it goes through a short signaling handshake before media starts flowing:

```mermaid
flowchart TD
    A([Browser clicks Connect]) --> B[Browser connects to /ws/signal]
    B --> C[Browser sends SDP offer\ndescribing its media capabilities]
    C --> D[Lumen creates a WebRTC session\nand replies with SDP answer]
    D --> E[Browser and server exchange\nICE candidates over WebSocket]
    E --> F{Can they reach\neach other directly?}
    F -- Yes --> G[Direct UDP connection\nDTLS handshake → SRTP set up]
    F -- No --> H[Traffic relayed through\nembedded TURN server]
    G --> I([H.264 video + Opus audio\nflow to browser over SRTP/UDP])
    H --> I
```

Once the SRTP connection is established, the WebSocket is no longer in the media path — all video, audio, and input flow directly over UDP.

---

## Components at a Glance

| Component | Role |
|-----------|------|
| **lumen-compositor** | Wayland compositor built on [Smithay](https://github.com/Smithay/smithay). Runs Wayland apps, captures frames, and injects input events. |
| **lumen-encode** | H.264 encoder. Uses VA-API (GPU, zero-copy) when available; falls back to x264 (software) automatically. |
| **lumen-audio** | Creates a virtual PipeWire audio sink (`lumen_capture`); captures audio routed to it and encodes it to Opus. |
| **lumen-webrtc** | Manages WebRTC sessions via [str0m](https://github.com/algesten/str0m). Handles ICE, DTLS, SRTP, and RTP packetization. |
| **lumen-web** | Axum HTTP server that serves the browser client and handles WebSocket signaling. |
| **lumen-turn** | Embedded TURN/STUN relay. Ensures streams work across NAT without an external relay service. |
| **lumen-gamepad** | Creates virtual input devices via `uinput` so browser gamepads appear as standard Linux input devices. |
| **web/** | Vanilla JavaScript browser client. Handles WebRTC setup, video rendering, and input capture. |

---

## Rendering Paths

Lumen automatically selects a rendering path based on whether a GPU render node is available:

```mermaid
flowchart LR
    Check{"GPU available?\n(/dev/dri/renderD128)"}
    Check -- Yes --> GPU["GPU path\nGlesRenderer → DMA-BUF\n→ VA-API encoder\n(zero-copy)"]
    Check -- No --> CPU["CPU path\nPixmanRenderer → RGBA buffer\n→ x264 encoder\n(software)"]
    GPU --> Out["H.264 bitstream\nto WebRTC"]
    CPU --> Out
```

The GPU path avoids any CPU memory copy and is strongly preferred. The CPU path works on any machine but uses significantly more CPU resources.
