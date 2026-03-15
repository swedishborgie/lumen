#!/usr/bin/env bash
# dev-test.sh — Build and run lumen, then attach a Wayland client to it for
# interactive testing.  Client detection picks the first available compositor
# or terminal from a priority list; override with LUMEN_LAUNCH or --launch.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LUMEN_BIN="$SCRIPT_DIR/target/debug/lumen"

# ── Build ──────────────────────────────────────────────────────────────────────
echo "==> Building lumen..."
cargo build --manifest-path "$SCRIPT_DIR/Cargo.toml"

# ── Auto-detect client ────────────────────────────────────────────────────────
# Skip detection if --launch or LUMEN_LAUNCH is already set.
LAUNCH_ARGS=()
if [[ -z "${LUMEN_LAUNCH:-}" ]] && [[ ! " $* " =~ " --launch " ]]; then
    # labwc is preferred: wlroots-based, supports zwlr_data_control_manager_v1
    # for clipboard bridging, and auto-detects the Wayland backend from
    # WAYLAND_DISPLAY.  weston needs --backend=wayland for nested mode.
    LAUNCH_CMD=""
    for candidate in labwc "weston --backend=wayland" foot kitty ghostty; do
        bin="${candidate%% *}"
        if command -v "$bin" &>/dev/null; then
            LAUNCH_CMD="$candidate"
            break
        fi
    done
    if [[ -n "$LAUNCH_CMD" ]]; then
        echo "==> Auto-detected client: $LAUNCH_CMD  (override with --launch or LUMEN_LAUNCH)"
        LAUNCH_ARGS=(--launch "$LAUNCH_CMD")
    else
        echo "==> WARN: no supported client found (labwc/weston/foot/kitty/ghostty)"
        echo "    Pass --launch <cmd> or set LUMEN_LAUNCH to specify a client"
    fi
fi

# ── Start lumen ────────────────────────────────────────────────────────────────
echo "==> Starting lumen  (Web UI: http://localhost:8080)"
echo "    Extra args: ${LAUNCH_ARGS[*]:-} $*"
exec "$LUMEN_BIN" "${LAUNCH_ARGS[@]}" "$@"

