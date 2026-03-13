#!/usr/bin/env bash
# dev-test.sh — Build and run lumen, then attach a terminal emulator to its
# Wayland socket for interactive testing.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LUMEN_BIN="$SCRIPT_DIR/target/debug/lumen"

# ── Build ──────────────────────────────────────────────────────────────────────
echo "==> Building lumen..."
cargo build --manifest-path "$SCRIPT_DIR/Cargo.toml"

# ── Auto-detect iGPU render node ──────────────────────────────────────────────
# Pass --dri-node automatically if the caller hasn't already provided one and
# LUMEN_DRI_NODE isn't set in the environment.
DRI_ARGS=()
if [[ -z "${LUMEN_DRI_NODE:-}" ]] && [[ ! " $* " =~ " --dri-node " ]]; then
    RENDER_NODE="$(ls /dev/dri/renderD* 2>/dev/null | head -1)"
    if [[ -n "$RENDER_NODE" ]]; then
        echo "==> Auto-detected render node: $RENDER_NODE (use --dri-node to override)"
        DRI_ARGS=(--dri-node "$RENDER_NODE")
    else
        echo "==> No /dev/dri/renderD* found, falling back to CPU (Pixman) renderer"
    fi
fi

# ── Auto-detect client and determine if clipboard bridge is needed ─────────────
# Do this before starting lumen so we can pass --inner-display at launch time.
# labwc is preferred: wlroots-based, supports zwlr_data_control_manager_v1 for
# clipboard bridging, runs borderless in nested mode (no window decorations),
# and auto-detects the Wayland backend from WAYLAND_DISPLAY.
INNER_DISPLAY_ARGS=()
TERM_CMD=""
for candidate in labwc weston foot kitty ghostty; do
    if command -v "$candidate" &>/dev/null; then
        TERM_CMD="$candidate"
        break
    fi
done
if [[ "$TERM_CMD" == "labwc" || "$TERM_CMD" == "weston" ]]; then
    # Use auto-discovery: the bridge scans XDG_RUNTIME_DIR for a Wayland socket
    # that advertises zwlr_data_control_manager_v1, so we don't need to predict
    # the socket name labwc will pick.
    INNER_DISPLAY_ARGS=(--inner-display auto)
    echo "==> $TERM_CMD detected: clipboard bridge will auto-discover inner socket"
fi

# ── Start lumen ────────────────────────────────────────────────────────────────

LOG_FILE="$(mktemp /tmp/lumen-XXXXXX.log)"
echo "==> Starting lumen (log: $LOG_FILE)"
echo "    Extra args: ${DRI_ARGS[*]} ${INNER_DISPLAY_ARGS[*]} $*"
"$LUMEN_BIN" "${DRI_ARGS[@]}" "${INNER_DISPLAY_ARGS[@]}" "$@" >"$LOG_FILE" 2>&1 &
LUMEN_PID=$!

# Ensure lumen is killed when this script exits
cleanup() {
    echo ""
    echo "==> Stopping lumen (PID $LUMEN_PID)..."
    kill "$LUMEN_PID" 2>/dev/null || true
    wait "$LUMEN_PID" 2>/dev/null || true
    if [[ -n "${TERM_PID:-}" ]]; then
        echo "==> Stopping $TERM_CMD (PID $TERM_PID)..."
        kill "$TERM_PID" 2>/dev/null || true
    fi
    echo "==> Done. Lumen log: $LOG_FILE"
    if [[ -n "${TERM_LOG:-}" ]]; then
        echo "    Terminal log: $TERM_LOG"
    fi
}
trap cleanup EXIT INT TERM

# ── Wait for Wayland socket ────────────────────────────────────────────────────
echo "==> Waiting for Wayland socket..."
WAYLAND_SOCK=""
for _ in $(seq 1 100); do
    if ! kill -0 "$LUMEN_PID" 2>/dev/null; then
        echo "ERROR: lumen exited unexpectedly. Last log lines:"
        tail -20 "$LOG_FILE"
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
    tail -20 "$LOG_FILE"
    exit 1
fi

echo "==> Wayland socket: $WAYLAND_SOCK  (lumen PID: $LUMEN_PID)"

# Verify the socket file actually exists in XDG_RUNTIME_DIR
RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
SOCKET_PATH="$RUNTIME_DIR/$WAYLAND_SOCK"
if [[ ! -S "$SOCKET_PATH" ]]; then
    echo "WARNING: socket file not found at $SOCKET_PATH — clients may not connect"
else
    echo "==> Socket file confirmed: $SOCKET_PATH"
fi

# ── Launch client ─────────────────────────────────────────────────────────────
# TERM_CMD was already detected above (before lumen start).
if [[ -z "$TERM_CMD" ]]; then
    echo "ERROR: no supported client found (labwc / weston / foot / kitty / ghostty)"
    exit 1
fi

TERM_LOG="$(mktemp /tmp/lumen-term-XXXXXX.log)"
echo "==> Launching $TERM_CMD on WAYLAND_DISPLAY=$WAYLAND_SOCK  (log: $TERM_LOG)"
echo "    Web UI at: http://localhost:8080  (or --bind to override)"
echo ""

# Build launch args per compositor:
# - labwc: auto-detects nested mode from WAYLAND_DISPLAY; no extra args needed.
# - weston: requires --backend=wayland for nested mode.
TERM_ARGS=()
if [[ "$TERM_CMD" == "weston" ]]; then
    TERM_ARGS=(--backend=wayland)
fi

# Unset DISPLAY so the client uses Wayland and not X11.
env -u DISPLAY \
    WAYLAND_DISPLAY="$WAYLAND_SOCK" \
    XDG_RUNTIME_DIR="$RUNTIME_DIR" \
    "$TERM_CMD" "${TERM_ARGS[@]}" >"$TERM_LOG" 2>&1 &
TERM_PID=$!

# Give the terminal a moment to start, then check it's still alive.
sleep 1
if ! kill -0 "$TERM_PID" 2>/dev/null; then
    echo "WARNING: $TERM_CMD exited immediately. Terminal log:"
    cat "$TERM_LOG"
fi

# ── Tail logs while running ────────────────────────────────────────────────────
echo "--- lumen output (Ctrl+C to stop) ---"
tail -f "$LOG_FILE" --pid="$LUMEN_PID" 2>/dev/null || true
