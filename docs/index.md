---
title: Home
layout: home
nav_order: 1
description: "Lumen — access your Linux desktop from any web browser, anywhere."
permalink: /
---

# Lumen

**Your Linux desktop, in any browser.**

Lumen gives you full, interactive access to a Linux desktop directly from a web browser — no software to install on the client, no plugins, no VPN. Open a tab, click Connect, and you're in.

---

## What Can You Do With It?

Whether you're accessing a workstation remotely, sharing a development environment with your team, or running applications on a powerful server from a lightweight device, Lumen gets out of the way and lets you work.

- **Full desktop access** — run any Linux application, desktop environment, or window manager, all accessible from the browser
- **Works on any device** — laptop, tablet, thin client, Chromebook; if the browser supports WebRTC, it works
- **No client software** — nothing to install, configure, or update on the device you're connecting from
- **Audio included** — system audio streams alongside the video, in sync
- **Full keyboard, mouse, and clipboard** — interact naturally; copy and paste works between your local machine and the remote session
- **Multiple simultaneous viewers** — more than one browser can connect to the same session at once

---

## Designed for Low Latency

Lumen streams your desktop over **WebRTC** — the same technology that powers real-time video calls in modern browsers. Video is encoded in H.264 (with hardware acceleration on Intel and AMD GPUs) and delivered over encrypted UDP, keeping latency low even over a network.

When hardware acceleration is available, frames travel from the GPU directly to the network without ever touching system RAM. On machines without a compatible GPU, software encoding kicks in automatically.

---

## Secure by Default

Access to the stream can be controlled in several ways:

| Mode | Description |
|------|-------------|
| **HTTP Basic (PAM)** | Username and password validated against system accounts |
| **Bearer token** | Preshared secret — simple and effective for reverse proxy setups |
| **OAuth2 / OIDC** | Integrate with your existing identity provider (Google, Okta, etc.) |

---

## Get Started

Choose an installation method to get up and running:

- [**Docker / Podman**](getting-started/docker) — Try Lumen in minutes with no host dependencies. Includes a bundled desktop and browser.
- [**Ubuntu / Debian**](getting-started/ubuntu) — Install via a native `.deb` package with systemd service integration.
- [**Fedora / RHEL**](getting-started/fedora) — Install via a native `.rpm` package with systemd service integration.

Curious how it works under the hood? See the [Architecture](architecture) page.
