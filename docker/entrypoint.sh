#!/usr/bin/env bash
# entrypoint.sh — Bootstrap lumen inside a container and launch labwc as the
# inner Wayland compositor, mirroring the logic in dev-test.sh.
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

# ── Auto-detect iGPU render node ──────────────────────────────────────────────
DRI_ARGS=()
if [[ -z "${LUMEN_DRI_NODE:-}" ]] && [[ ! " $* " =~ " --dri-node " ]]; then
    RENDER_NODE="$(ls /dev/dri/renderD* 2>/dev/null | head -1)"
    if [[ -n "$RENDER_NODE" ]]; then
        echo "==> Auto-detected render node: $RENDER_NODE"
        DRI_ARGS=(--dri-node "$RENDER_NODE")
    else
        echo "==> No /dev/dri/renderD* found — using CPU (Pixman) renderer"
    fi
fi

# ── Auto-detect TURN external IP ──────────────────────────────────────────────
# The TURN relay address must be an IP the browser can reach.  In a Podman
# container the port mappings (-p 3478:3478/udp, -p 50000-50010:50000-50010/udp)
# are exposed on every host interface, so we need to advertise the same IP that
# the container's routing stack selects as its outbound address — browsers can
# reach that IP via the host port mappings.
#
# This is the same probe lumen uses internally for ICE host candidates.
#
# Override by setting LUMEN_TURN_EXTERNAL_IP before calling this script.
if [[ -z "${LUMEN_TURN_EXTERNAL_IP:-}" ]]; then
    DETECTED_IP="$(ip route get 8.8.8.8 2>/dev/null \
        | awk '/src/ { for(i=1;i<=NF;i++) if ($i=="src") { print $(i+1); exit } }' \
        || true)"
    if [[ -n "$DETECTED_IP" ]] && [[ "$DETECTED_IP" != "127.0.0.1" ]] && [[ "$DETECTED_IP" != "::1" ]]; then
        export LUMEN_TURN_EXTERNAL_IP="$DETECTED_IP"
        echo "==> Auto-detected TURN external IP: $LUMEN_TURN_EXTERNAL_IP"
    else
        echo "==> WARN: Could not detect a non-loopback TURN IP; set LUMEN_TURN_EXTERNAL_IP manually"
    fi
fi

# ── Clipboard bridge: labwc supports zwlr_data_control_manager_v1 ─────────────
INNER_DISPLAY_ARGS=(--inner-display auto)

# ── Web asset path ────────────────────────────────────────────────────────────
WEB_ARGS=()
if [[ -d /opt/lumen/web ]]; then
    WEB_ARGS=(--static-dir /opt/lumen/web)
fi

# ── Start lumen ────────────────────────────────────────────────────────────────
LOG_FILE="$(mktemp /tmp/lumen-XXXXXX.log)"
echo "==> Starting lumen (log: $LOG_FILE)"
echo "    Args: ${DRI_ARGS[*]:-} ${INNER_DISPLAY_ARGS[*]:-} ${WEB_ARGS[*]:-} $*"

"$LUMEN_BIN" \
    "${DRI_ARGS[@]}" \
    "${INNER_DISPLAY_ARGS[@]}" \
    "${WEB_ARGS[@]}" \
    "$@" >"$LOG_FILE" 2>&1 &
LUMEN_PID=$!

# ── Cleanup on exit ───────────────────────────────────────────────────────────
cleanup() {
    echo ""
    echo "==> Stopping lumen (PID $LUMEN_PID)..."
    kill "$LUMEN_PID" 2>/dev/null || true
    wait "$LUMEN_PID" 2>/dev/null || true
    if [[ -n "${LABWC_PID:-}" ]]; then
        echo "==> Stopping labwc (PID $LABWC_PID)..."
        kill "$LABWC_PID" 2>/dev/null || true
    fi
    echo "==> Done. Lumen log: $LOG_FILE"
}
trap cleanup EXIT INT TERM

# ── Wait for Wayland socket ────────────────────────────────────────────────────
echo "==> Waiting for Wayland socket..."
WAYLAND_SOCK=""
for _ in $(seq 1 150); do
    if ! kill -0 "$LUMEN_PID" 2>/dev/null; then
        echo "ERROR: lumen exited unexpectedly. Last log lines:"
        tail -30 "$LOG_FILE"
        exit 1
    fi
    WAYLAND_SOCK=$(awk -F'Wayland socket: ' 'NF>1{print $2; exit}' "$LOG_FILE" 2>/dev/null \
        | tr -d '[:space:]')
    if [[ -n "$WAYLAND_SOCK" ]]; then
        break
    fi
    sleep 0.2
done

if [[ -z "$WAYLAND_SOCK" ]]; then
    echo "ERROR: timed out waiting for Wayland socket. Last log lines:"
    tail -30 "$LOG_FILE"
    exit 1
fi

echo "==> Wayland socket: $WAYLAND_SOCK  (lumen PID: $LUMEN_PID)"
echo "==> Web UI available at: http://localhost:8080"
echo ""

# ── Launch labwc as the inner compositor ──────────────────────────────────────
LABWC_LOG="$(mktemp /tmp/lumen-labwc-XXXXXX.log)"
echo "==> Launching labwc on WAYLAND_DISPLAY=$WAYLAND_SOCK  (log: $LABWC_LOG)"

env -u DISPLAY \
    WAYLAND_DISPLAY="$WAYLAND_SOCK" \
    XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
    labwc >"$LABWC_LOG" 2>&1 &
LABWC_PID=$!

# Give labwc a moment then verify it started
sleep 1
if ! kill -0 "$LABWC_PID" 2>/dev/null; then
    echo "WARNING: labwc exited immediately. labwc log:"
    cat "$LABWC_LOG"
fi

# ── Tail lumen output until it exits ─────────────────────────────────────────
echo "--- lumen output (Ctrl+C to stop) ---"
tail -f "$LOG_FILE" --pid="$LUMEN_PID" 2>/dev/null || true
