#!/usr/bin/env bash
# dev-test.sh — Build and run lumen, then attach a terminal emulator to its
# Wayland socket for interactive testing.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LUMEN_BIN="$SCRIPT_DIR/target/debug/lumen"

# ── Build ──────────────────────────────────────────────────────────────────────
echo "==> Building lumen..."
cargo build --manifest-path "$SCRIPT_DIR/Cargo.toml"

# ── Start lumen ────────────────────────────────────────────────────────────────
LOG_FILE="$(mktemp /tmp/lumen-XXXXXX.log)"
echo "==> Starting lumen (log: $LOG_FILE)"
echo "    Extra args: $*"
"$LUMEN_BIN" "$@" >"$LOG_FILE" 2>&1 &
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

# ── Launch terminal emulator ───────────────────────────────────────────────────
# Prefer foot (minimal, reliable Wayland-native), then kitty, then ghostty.
TERM_CMD=""
for candidate in ghostty foot kitty; do
    if command -v "$candidate" &>/dev/null; then
        TERM_CMD="$candidate"
        break
    fi
done
if [[ -z "$TERM_CMD" ]]; then
    echo "ERROR: no supported terminal emulator found (foot / kitty / ghostty)"
    exit 1
fi

TERM_LOG="$(mktemp /tmp/lumen-term-XXXXXX.log)"
echo "==> Launching $TERM_CMD on WAYLAND_DISPLAY=$WAYLAND_SOCK  (log: $TERM_LOG)"
echo "    Web UI at: http://localhost:8080  (or --bind to override)"
echo ""

# Unset DISPLAY so the terminal uses Wayland and not X11.
# WAYLAND_DEBUG=client logs every protocol message the terminal sends/receives.
env -u DISPLAY \
    WAYLAND_DISPLAY="$WAYLAND_SOCK" \
    XDG_RUNTIME_DIR="$RUNTIME_DIR" \
    "$TERM_CMD" >"$TERM_LOG" 2>&1 &
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
