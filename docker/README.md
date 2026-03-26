# Lumen — Docker / Podman Container

A container image that bundles a complete desktop environment — **labwc** or **KDE Plasma**, **XWayland**, and **Firefox** — on top of Ubuntu. The Lumen WebRTC stream is accessible at **http://localhost:8080** from the host.

---

## Pull the Image

Pre-built images are available on the GitHub Container Registry:

```bash
# labwc (lightweight Wayland compositor) — recommended
podman pull ghcr.io/swedishborgie/lumen:latest-labwc

# KDE Plasma 6 (kwin_wayland + plasmashell)
podman pull ghcr.io/swedishborgie/lumen:latest-kde
```

| Image tag                      | Desktop                                   | Terminal |
| ------------------------------ | ----------------------------------------- | -------- |
| `latest-labwc` _(recommended)_ | labwc — lightweight wlroots compositor    | foot     |
| `latest-kde`                   | KDE Plasma 6 (kwin_wayland + plasmashell) | Konsole  |

---

## Run

`--network host` is the recommended networking mode — it bypasses NAT and avoids WebRTC UDP flow issues that can occur with port mapping:

```bash
podman run --rm -it --device /dev/dri --network host ghcr.io/swedishborgie/lumen:latest-labwc
```

### No GPU (CPU / Pixman renderer)

```bash
podman run --rm -it --network host ghcr.io/swedishborgie/lumen:latest-labwc
```

### AMD or Intel GPU passthrough

Pass the entire DRI group so lumen can use VA-API hardware encoding:

```bash
podman run --rm -it \
    --device /dev/dri \
    --security-opt label=disable \
    --network host \
    ghcr.io/swedishborgie/lumen:latest-labwc
```

### Gamepad / joystick passthrough

Lumen forwards browser gamepad input to virtual Linux input devices via `uinput`.
Pass `/dev/uinput` and add the `input` group so the container can create those devices:

```bash
podman run --rm -it \
    --device /dev/uinput \
    --group-add input \
    --network host \
    ghcr.io/swedishborgie/lumen:latest-labwc
```

The `uinput` kernel module must be loaded on the **host** before starting the container
(it almost always is on modern Linux systems, but you can verify with `lsmod | grep uinput`
or load it with `sudo modprobe uinput`).

The `input` group is granted write access to `/dev/uinput` by the udev rule installed with the package (`pkgs/70-lumen-uinput.rules`). Combine `--device /dev/uinput --group-add input` with any other
flags (GPU, network) as needed.

If `/dev/uinput` is not passed through, lumen starts normally and gamepad support is
simply disabled — a warning is printed in the container log.

### NVIDIA GPU passthrough (CDI)

NVIDIA GPU passthrough requires the [CDI plugin](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html) on the host:

```bash
podman run --rm -it \
    --device nvidia.com/gpu=all \
    --security-opt label=disable \
    --network host \
    ghcr.io/swedishborgie/lumen:latest-labwc
```

---

## WebRTC and the embedded TURN server

WebRTC requires both peers to be able to reach each other's ICE candidates. When running inside a container (Podman/Docker), the container's virtual NIC IP is not directly reachable from the browser on the host, so direct ICE candidates fail.

Lumen includes an **embedded TURN server** (port 3478/udp) that acts as a relay. Both the browser and lumen itself connect to it as TURN clients. The relay address is `127.0.0.1` (localhost), which is always reachable from the host as long as you map the ports.

Required port mappings:

| Port          | Protocol | Purpose                            |
| ------------- | -------- | ---------------------------------- |
| `8080`        | TCP      | HTTP server + WebSocket signaling  |
| `3478`        | UDP      | TURN control (allocation requests) |
| `50000–50010` | UDP      | TURN relay data channels           |

The browser receives TURN credentials automatically via `/api/config` — no manual configuration required.

To **disable** the TURN server (e.g. if lumen is accessed directly on the host without containers):

```bash
podman run ... -e LUMEN_TURN_PORT=0 ghcr.io/swedishborgie/lumen:latest-labwc
```

---

## What's inside

### labwc image (`DESKTOP=labwc`, default)

| Component          | Purpose                                                     |
| ------------------ | ----------------------------------------------------------- |
| `lumen`            | The compositor/streamer binary                              |
| `labwc`            | Inner Wayland compositor (the desktop you stream)           |
| `xwayland`         | XWayland bridge for X11 apps inside labwc                   |
| `firefox`          | Browser — auto-started by labwc on launch                   |
| `foot`             | Terminal emulator — available in the labwc right-click menu |
| `xclock` / `xeyes` | X11 test utilities (`x11-apps` package)                     |
| `pipewire`         | Audio server for audio capture                              |

### KDE image (`DESKTOP=kde`)

| Component          | Purpose                                       |
| ------------------ | --------------------------------------------- |
| `lumen`            | The compositor/streamer binary                |
| `kwin_wayland`     | KDE window manager / inner Wayland compositor |
| `plasmashell`      | KDE Plasma desktop shell                      |
| `xwayland`         | XWayland bridge for X11 apps                  |
| `firefox`          | Browser — auto-started on launch              |
| `konsole`          | KDE terminal emulator                         |
| `xclock` / `xeyes` | X11 test utilities (`x11-apps` package)       |
| `pipewire`         | Audio server for audio capture                |

> **Gamepad support** requires passing `/dev/uinput` to the container (see [Gamepad passthrough](#gamepad--joystick-passthrough) above).

---

## Accessing the stream

Once the container is running, open a browser on the host and navigate to:

```
http://localhost:8080
```

You will see the Lumen web UI. Click **Connect** to start receiving the WebRTC video stream of the desktop.

---

## Testing X11 apps

Open a terminal (foot in labwc, Konsole in KDE) and run:

```bash
xclock &
xeyes &
```

Both apps should appear as X11 windows rendered via XWayland inside the labwc session.

---

## Environment variables

| Variable                 | Default           | Description                                                                                                                                                               |
| ------------------------ | ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `DESKTOP`                | `labwc`           | Desktop environment — `labwc` or `kde`. Set at image build time; can be overridden at runtime with `-e DESKTOP=kde` when using an image built with the matching packages. |
| `LUMEN_BIND`             | `0.0.0.0:8080`    | HTTP server bind address                                                                                                                                                  |
| `LUMEN_DRI_NODE`         | _(auto-detected)_ | Override the DRI render node (e.g. `/dev/dri/renderD128`)                                                                                                                 |
| `LUMEN_TURN_PORT`        | `3478`            | TURN server UDP port. Set to `0` to disable.                                                                                                                              |
| `LUMEN_TURN_EXTERNAL_IP` | `127.0.0.1`       | IP advertised as the TURN relay address. Change to the host LAN IP for remote access.                                                                                     |
| `LUMEN_TURN_USERNAME`    | `lumen`           | TURN credential username                                                                                                                                                  |
| `LUMEN_TURN_PASSWORD`    | `lumenpass`       | TURN credential password                                                                                                                                                  |
| `LUMEN_TURN_MIN_PORT`    | `50000`           | Start of TURN relay UDP port range                                                                                                                                        |
| `LUMEN_TURN_MAX_PORT`    | `50010`           | End of TURN relay UDP port range                                                                                                                                          |

---

## Remote access (LAN / internet)

To allow WebRTC connections from other machines, set `LUMEN_TURN_EXTERNAL_IP` to the host's reachable IP:

```bash
podman run --rm -it \
    --device /dev/dri \
    --security-opt label=disable \
    --network host \
    -e LUMEN_TURN_EXTERNAL_IP=192.168.1.100 \
    ghcr.io/swedishborgie/lumen:latest-labwc
```

---

## SELinux / AppArmor note

On hosts with SELinux or AppArmor enforcement, GPU device passthrough may require:

```bash
--security-opt label=disable   # SELinux
--security-opt apparmor=unconfined  # AppArmor
```

---

## Building from Source

If you need to build the image locally (e.g. to test local changes), use the Dockerfile in the repository root:

```bash
# labwc (lightweight Wayland compositor)
podman build -f docker/Dockerfile -t lumen:latest .

# KDE Plasma 6 (kwin_wayland + plasmashell)
podman build --build-arg DESKTOP=kde -f docker/Dockerfile -t lumen:kde .
```

The build has three stages:

1. **planner** — runs `cargo chef prepare` to compute a dependency recipe
2. **builder** — compiles all Rust and native C/C++ dependencies, then the application
3. **runtime** — minimal Ubuntu image with the selected desktop, XWayland, Firefox, PipeWire, and the compiled `lumen` binary

> **Tip:** The first build takes a while (Rust + Smithay + FFmpeg bindings). Subsequent builds that only change application source skip the dependency compilation step entirely — Podman reuses the cached layer.

> **KDE and systemd:** KDE Plasma 6's `startplasma-wayland` automatically detects whether systemd is available and falls back to direct launch mode when it is not (the normal case inside a container).
