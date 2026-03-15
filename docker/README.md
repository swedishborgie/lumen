# Lumen — Docker / Podman Container

A multi-stage container image that builds Lumen from source and runs it inside a minimal Ubuntu 24.04 desktop (labwc + XWayland + Firefox). The Lumen WebRTC stream is accessible at **http://localhost:8080** from the host.

---

## Build

```bash
# From the repo root
podman build -f docker/Dockerfile -t lumen:latest .
```

The build has two stages:
1. **builder** — installs the full Rust toolchain and all native C/C++ dependencies, compiles the release binary
2. **runtime** — minimal Ubuntu image with labwc, XWayland, Firefox, xclock/xeyes, PulseAudio, and the compiled `lumen` binary

> **Tip:** The first build will take a while (Rust + Smithay compile time). Subsequent builds use the Docker/Podman layer cache for unchanged dependencies.

---

## Run

> **Podman users — host networking:** Podman's default NAT networking can interfere with WebRTC UDP flows even when ports are mapped. If you experience connection issues, use `--network host` to bypass NAT entirely (port mapping flags are then unnecessary):
> ```bash
> podman run --rm -it --network host lumen:latest
> ```

### No GPU (CPU / Pixman renderer)

```bash
podman run --rm -it \
    -p 8080:8080 \
    -p 3478:3478/udp \
    -p 50000-50010:50000-50010/udp \
    lumen:latest
```

### AMD or Intel GPU passthrough

Pass the entire DRI group so lumen can use VA-API hardware encoding:

```bash
podman run --rm -it \
    --device /dev/dri \
    --security-opt label=disable \
    -p 8080:8080 \
    -p 3478:3478/udp \
    -p 50000-50010:50000-50010/udp \
    lumen:latest
```

### Gamepad / joystick passthrough

Lumen forwards browser gamepad input to virtual Linux input devices via `uinput`.
Pass `/dev/uinput` so the container can create those devices:

```bash
podman run --rm -it \
    --device /dev/uinput \
    -p 8080:8080 \
    -p 3478:3478/udp \
    -p 50000-50010:50000-50010/udp \
    lumen:latest
```

The `uinput` kernel module must be loaded on the **host** before starting the container
(it almost always is on modern Linux systems, but you can verify with `lsmod | grep uinput`
or load it with `sudo modprobe uinput`).  Combine `--device /dev/uinput` with any other
flags (GPU, network) as needed.

If `/dev/uinput` is not passed through, lumen starts normally and gamepad support is
simply disabled — a warning is printed in the container log.

### NVIDIA GPU passthrough (CDI)

NVIDIA GPU passthrough requires the [CDI plugin](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html) on the host:

```bash
podman run --rm -it \
    --device nvidia.com/gpu=all \
    --security-opt label=disable \
    -p 8080:8080 \
    -p 3478:3478/udp \
    -p 50000-50010:50000-50010/udp \
    lumen:latest
```

---

## WebRTC and the embedded TURN server

WebRTC requires both peers to be able to reach each other's ICE candidates. When running inside a container (Podman/Docker), the container's virtual NIC IP is not directly reachable from the browser on the host, so direct ICE candidates fail.

Lumen includes an **embedded TURN server** (port 3478/udp) that acts as a relay. Both the browser and lumen itself connect to it as TURN clients. The relay address is `127.0.0.1` (localhost), which is always reachable from the host as long as you map the ports.

Required port mappings:

| Port | Protocol | Purpose |
|------|----------|---------|
| `8080` | TCP | HTTP server + WebSocket signaling |
| `3478` | UDP | TURN control (allocation requests) |
| `50000–50010` | UDP | TURN relay data channels |

The browser receives TURN credentials automatically via `/api/config` — no manual configuration required.

To **disable** the TURN server (e.g. if lumen is accessed directly on the host without containers):

```bash
podman run ... -e LUMEN_TURN_PORT=0 lumen:latest
```

---

## What's inside

| Component | Purpose |
|-----------|---------|
| `lumen` | The compositor/streamer binary |
| `labwc` | Inner Wayland compositor (the desktop you stream) |
| `xwayland` | XWayland bridge for X11 apps inside labwc |
| `firefox` | Browser — auto-started by labwc on launch |
| `foot` | Terminal emulator — available in the labwc right-click menu |
| `xclock` / `xeyes` | X11 test utilities (`x11-apps` package) |
| `pulseaudio` | Audio server for audio capture |

> **Gamepad support** requires passing `/dev/uinput` to the container (see [Gamepad passthrough](#gamepad--joystick-passthrough) above).

---

## Accessing the stream

Once the container is running, open a browser on the host and navigate to:

```
http://localhost:8080
```

You will see the Lumen web UI. Click **Connect** to start receiving the WebRTC video stream of the labwc desktop.

---

## Testing X11 apps

Open a terminal inside labwc (right-click the desktop → Terminal, if configured) and run:

```bash
xclock &
xeyes &
```

Both apps should appear as X11 windows rendered via XWayland inside the labwc session.

---

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LUMEN_BIND` | `0.0.0.0:8080` | HTTP server bind address |
| `LUMEN_DRI_NODE` | *(auto-detected)* | Override the DRI render node (e.g. `/dev/dri/renderD128`) |
| `LUMEN_TURN_PORT` | `3478` | TURN server UDP port. Set to `0` to disable. |
| `LUMEN_TURN_EXTERNAL_IP` | `127.0.0.1` | IP advertised as the TURN relay address. Change to the host LAN IP for remote access. |
| `LUMEN_TURN_USERNAME` | `lumen` | TURN credential username |
| `LUMEN_TURN_PASSWORD` | `lumenpass` | TURN credential password |
| `LUMEN_TURN_MIN_PORT` | `50000` | Start of TURN relay UDP port range |
| `LUMEN_TURN_MAX_PORT` | `50010` | End of TURN relay UDP port range |

---

## Remote access (LAN / internet)

To allow WebRTC connections from other machines, set `LUMEN_TURN_EXTERNAL_IP` to the host's reachable IP:

```bash
podman run --rm -it \
    --device /dev/dri \
    --security-opt label=disable \
    -p 8080:8080 \
    -p 3478:3478/udp \
    -p 50000-50010:50000-50010/udp \
    -e LUMEN_TURN_EXTERNAL_IP=192.168.1.100 \
    lumen:latest
```

---

## SELinux / AppArmor note

On hosts with SELinux or AppArmor enforcement, GPU device passthrough may require:

```bash
--security-opt label=disable   # SELinux
--security-opt apparmor=unconfined  # AppArmor
```
