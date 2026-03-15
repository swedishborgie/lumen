#!/usr/bin/env bash
# entrypoint.sh — Bootstrap lumen inside a container.
#
# GPU passthrough (AMD/Intel):
#   podman run --device /dev/dri ...
# GPU passthrough (NVIDIA, CDI):
#   podman run --device nvidia.com/gpu=all ...
set -euo pipefail

LUMEN_BIN="${LUMEN_BIN:-/usr/local/bin/lumen}"

# ── XDG_RUNTIME_DIR ────────────────────────────────────────────────────────────
# Prefer /run/user/<uid>; fall back to /tmp/runtime for rootless containers
# where /run/user may not be writable.
UID_VAL="$(id -u)"
if [[ -w "/run/user" ]] || mkdir -p "/run/user/$UID_VAL" 2>/dev/null; then
    export XDG_RUNTIME_DIR="/run/user/$UID_VAL"
else
    export XDG_RUNTIME_DIR="/tmp/runtime-$UID_VAL"
fi
mkdir -p "$XDG_RUNTIME_DIR"
chmod 0700 "$XDG_RUNTIME_DIR"

echo "==> XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"

# ── D-Bus ──────────────────────────────────────────────────────────────────────
if command -v dbus-daemon &>/dev/null && [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]]; then
    eval "$(dbus-launch --sh-syntax)"
    echo "==> D-Bus session started: $DBUS_SESSION_BUS_ADDRESS"
fi

# ── PulseAudio ────────────────────────────────────────────────────────────────
if command -v pulseaudio &>/dev/null; then
    pulseaudio --daemonize --exit-idle-time=-1 --log-level=warn 2>/dev/null || true
    echo "==> PulseAudio started"
fi

# ── uinput (virtual gamepad devices) ─────────────────────────────────────────
# Try to load the uinput kernel module.  This succeeds when the container has
# CAP_SYS_MODULE (e.g. --privileged) or the module is already loaded on the host.
modprobe uinput 2>/dev/null || true
if [[ -c /dev/uinput ]]; then
    echo "==> /dev/uinput available — gamepad support enabled"
else
    echo "==> WARN: /dev/uinput not found — gamepad support disabled"
    echo "    Pass --device /dev/uinput to enable gamepad forwarding"
fi

# ── Web asset path ────────────────────────────────────────────────────────────
WEB_ARGS=()
if [[ -d /opt/lumen/web ]]; then
    WEB_ARGS=(--static-dir /opt/lumen/web)
fi

# ── Start lumen ────────────────────────────────────────────────────────────────
# DRI node detection, TURN IP detection, clipboard bridge setup, and labwc
# launch are all handled internally by lumen.
echo "==> Starting lumen  (Web UI: http://localhost:8080)"
echo "    Args: ${WEB_ARGS[*]:-} --launch labwc $*"
exec "$LUMEN_BIN" \
    "${WEB_ARGS[@]}" \
    --launch labwc \
    "$@"

