---
title: Getting Started
layout: default
nav_order: 3
has_children: true
description: "Install and configure Lumen on your system."
---

# Getting Started

Lumen can be installed in several ways depending on your environment and preferences.

| Method | Best for |
|--------|----------|
| [Docker / Podman](podman) | Quickest way to try Lumen — no host dependencies required |
| [Ubuntu / Debian](ubuntu) | Native `.deb` package with systemd integration |
| [Fedora / RHEL](fedora) | Native `.rpm` package with systemd integration |

---

## What You'll Need

Regardless of installation method, you'll need:

- A machine running Linux (physical or virtual)
- A modern web browser on any device to connect to the stream
- Ports **8080/TCP**, **3478/UDP**, and **50000–50010/UDP** accessible from the browser's network

{: .note }
The embedded TURN server handles NAT traversal automatically. If you're accessing Lumen from the same machine it runs on, no special network configuration is needed.
