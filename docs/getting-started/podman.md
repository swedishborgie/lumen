---
title: Docker / Podman
layout: default
parent: Getting Started
nav_order: 1
description: "Run Lumen inside a Podman container with a bundled desktop environment (labwc or KDE) and Firefox."
---

# Docker / Podman
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

The Lumen container image bundles a complete desktop environment — **labwc** or **KDE Plasma**, **XWayland**, and **Firefox** — on top of Ubuntu. It's the fastest way to try Lumen without installing anything on your host system.

{: .note }
The instructions below use `podman`. `docker` works as a drop-in replacement for every command.

---

## Build the Image

From the repository root:

```bash
# Default — labwc (lightweight Wayland compositor)
podman build -f docker/Dockerfile -t lumen:latest .

# KDE Plasma 6 (kwin_wayland + plasmashell)
podman build --build-arg DESKTOP=kde -f docker/Dockerfile -t lumen:kde .
```

The `DESKTOP` build argument selects the desktop environment bundled into the image:

| Value | Desktop | Terminal |
|-------|---------|----------|
| `labwc` *(default)* | labwc — lightweight wlroots compositor | foot |
| `kde` | KDE Plasma 6 (kwin_wayland + plasmashell) | Konsole |

{: .tip }
The first build compiles Rust and all native C/C++ dependencies and will take several minutes. Subsequent builds that only change application source code reuse the cached dependency layer and are much faster. The dependency layer is only invalidated when `Cargo.toml` or `Cargo.lock` changes.

---

## Run the Container

### Recommended: host networking

Podman's default NAT networking can interfere with WebRTC UDP flows even when ports are mapped. Using `--network host` bypasses NAT entirely and is the recommended approach:

```bash
podman run --rm -it --device /dev/dri --network host lumen:latest
```

Open `http://localhost:8080` in a browser and click **Connect**.

### No GPU (CPU / Pixman renderer)

Works on any Linux machine without a GPU:

```bash
podman run --rm -it --network host lumen:latest
```

### AMD or Intel GPU passthrough

Pass the DRI device group to enable VA-API hardware encoding:

```bash
podman run --rm -it \
    --device /dev/dri \
    --security-opt label=disable \
    --network host \
    lumen:latest
```

### NVIDIA GPU passthrough (CDI)

Requires the [NVIDIA Container Toolkit (CDI)](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html) on the host:

```bash
podman run --rm -it \
    --device nvidia.com/gpu=all \
    --security-opt label=disable \
    --network host \
    lumen:latest
```

### Gamepad / joystick passthrough

Lumen can forward browser gamepad input to virtual Linux input devices via `uinput`. Pass `/dev/uinput` and add the `input` group to enable this:

```bash
podman run --rm -it \
    --device /dev/uinput \
    --group-add input \
    --network host \
    lumen:latest
```

{: .note }
The `uinput` kernel module must be loaded on the host (`lsmod | grep uinput`). Load it with `sudo modprobe uinput` if not. The `input` group is granted write access to `/dev/uinput` by the udev rule installed with the package (`pkgs/70-lumen-uinput.rules`). If `/dev/uinput` is not passed, Lumen starts normally and gamepad support is simply disabled.

You can combine `--device /dev/uinput --group-add input` with GPU passthrough flags as needed.

### Port mapping (alternative to host networking)

If you prefer explicit port mapping instead of `--network host`:

```bash
podman run --rm -it \
    --device /dev/dri \
    --security-opt label=disable \
    -p 8080:8080 \
    -p 3478:3478/udp \
    -p 50000-50010:50000-50010/udp \
    lumen:latest
```

---

## Port Reference

| Port | Protocol | Purpose |
|------|----------|---------|
| `8080` | TCP | HTTP server + WebSocket signaling |
| `3478` | UDP | TURN server (NAT traversal control) |
| `50000–50010` | UDP | TURN relay data channels |

The embedded TURN server handles NAT traversal automatically. The browser receives TURN credentials via `/api/config` — no manual configuration is required.

To **disable** the TURN server:

```bash
podman run ... -e LUMEN_TURN_PORT=0 lumen:latest
```

---

## Remote Access

To allow WebRTC connections from other machines on your network, set `LUMEN_TURN_EXTERNAL_IP` to the host's reachable IP address:

```bash
podman run --rm -it \
    --device /dev/dri \
    --security-opt label=disable \
    --network host \
    -e LUMEN_TURN_EXTERNAL_IP=192.168.1.100 \
    lumen:latest
```

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LUMEN_BIND` | `0.0.0.0:8080` | HTTP server bind address |
| `LUMEN_DRI_NODE` | *(auto-detected)* | Override the DRI render node (e.g. `/dev/dri/renderD128`) |
| `LUMEN_TURN_PORT` | `3478` | TURN server UDP port. Set to `0` to disable. |
| `LUMEN_TURN_EXTERNAL_IP` | `127.0.0.1` | IP advertised as the TURN relay address |
| `LUMEN_TURN_USERNAME` | `lumen` | TURN credential username |
| `LUMEN_TURN_PASSWORD` | `lumenpass` | TURN credential password |
| `LUMEN_TURN_MIN_PORT` | `50000` | Start of TURN relay UDP port range |
| `LUMEN_TURN_MAX_PORT` | `50010` | End of TURN relay UDP port range |
| `LUMEN_WIDTH` | `1920` | Output width in pixels |
| `LUMEN_HEIGHT` | `1080` | Output height in pixels |
| `LUMEN_FPS` | `30.0` | Target frames per second |
| `LUMEN_VIDEO_BITRATE_KBPS` | `4000` | Video encoder target bitrate (kbps) |

---

## SELinux / AppArmor Note

On hosts with SELinux or AppArmor enforcement, GPU device passthrough may require additional flags:

```bash
--security-opt label=disable        # SELinux
--security-opt apparmor=unconfined  # AppArmor
```

---

## What's Inside the Image

### labwc image (`DESKTOP=labwc`, default)

| Component | Purpose |
|-----------|---------|
| `lumen` | The compositor/streamer binary |
| `labwc` | Inner Wayland compositor (the desktop you stream) |
| `xwayland` | XWayland bridge for X11 apps |
| `firefox` | Browser — auto-started by labwc on launch |
| `foot` | Terminal emulator (available in the labwc right-click menu) |
| `xclock` / `xeyes` | X11 test utilities |
| `pulseaudio` | Audio server for audio capture |

### KDE image (`DESKTOP=kde`)

| Component | Purpose |
|-----------|---------|
| `lumen` | The compositor/streamer binary |
| `kwin_wayland` | KDE window manager / inner Wayland compositor |
| `plasmashell` | KDE Plasma desktop shell |
| `xwayland` | XWayland bridge for X11 apps |
| `firefox` | Browser — auto-started on launch |
| `konsole` | KDE terminal emulator |
| `xclock` / `xeyes` | X11 test utilities |
| `pulseaudio` | Audio server for audio capture |
