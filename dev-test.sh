#!/usr/bin/env bash
# dev-test.sh — Build and run lumen, then attach a Wayland client to it for
# interactive testing.  Client detection picks the first available compositor
# or terminal from a priority list; override with LUMEN_LAUNCH or --launch.
set -euo pipefail

if [ -f secret.env ]; then
    echo "==> Loading secrets from secret.env (not committed to git)"
    source secret.env
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LUMEN_BIN="$SCRIPT_DIR/target/debug/lumen"

# ── Build ──────────────────────────────────────────────────────────────────────
echo "==> Building lumen..."
cargo build --manifest-path "$SCRIPT_DIR/Cargo.toml"

export QT_QPA_PLATFORM=wayland
export XDG_CURRENT_DESKTOP=KDE
export XDG_SESSION_TYPE=wayland
export KDE_SESSION_VERSION=6
# NixOS ships plasma-applications.menu; this prefix makes KDE find it.
export XDG_MENU_PREFIX=plasma-
# Suppress the Plasma splash screen in dev/test sessions.
export PLASMA_SKIP_SPLASH=1

# ── Auto-detect client ────────────────────────────────────────────────────────
# Skip detection if --launch or LUMEN_LAUNCH is already set.
LAUNCH_ARGS=()
if [[ -z "${LUMEN_LAUNCH:-}" ]] && [[ ! " $* " =~ " --launch " ]]; then
    # startplasma-wayland handles kbuildsycoca, polkit, and PlasmaWindowManagement
    # setup, but does NOT reliably redirect WAYLAND_DISPLAY to kwin's nested socket
    # before launching child apps (they end up connecting to lumen's socket directly).
    # The --desktop kde preset uses explicit kwin socket management instead; this
    # dev-test fallback uses startplasma-wayland only when it is available and the
    # caller hasn't passed --desktop or --launch explicitly.
    #
    # labwc is the next fallback: wlroots-based, supports
    # zwlr_data_control_manager_v1 for clipboard bridging.
    # weston needs --backend=wayland for nested mode.
    LAUNCH_CMD=""
    LAUNCH_DESC=""
    if command -v startplasma-wayland &>/dev/null; then
        LAUNCH_DESC="startplasma-wayland (KDE desktop)"
        LAUNCH_CMD="dbus-run-session startplasma-wayland"
    elif command -v kwin_wayland &>/dev/null && command -v plasmashell &>/dev/null; then
        # Fallback for non-NixOS systems where startplasma-wayland is absent.
        LAUNCH_DESC="kwin_wayland + plasmashell (KDE desktop)"
        LAUNCH_CMD='dbus-run-session sh -c '"'"'
            export KDE_FULL_SESSION=true
            export DESKTOP_SESSION=plasma
            kwin_wayland --socket kwin-wayland --no-lockscreen &
            KWIN_PID=$!
            RUNTIME="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
            i=0
            while [ $i -lt 30 ] && [ ! -S "$RUNTIME/kwin-wayland" ]; do
                sleep 0.5; i=$((i+1))
            done
            i=0
            while [ $i -lt 20 ] && ! dbus-send --session --print-reply \
                    --dest=org.freedesktop.DBus / \
                    org.freedesktop.DBus.GetNameOwner \
                    string:org.kde.KWin >/dev/null 2>&1; do
                sleep 0.5; i=$((i+1))
            done
            POLKIT=$(command -v polkit-kde-authentication-agent-1 2>/dev/null || true)
            for a in "$POLKIT" /usr/lib/libexec/polkit-kde-authentication-agent-1 /usr/libexec/polkit-kde-authentication-agent-1; do
                [ -x "$a" ] && { "$a" & break; }
            done
            WAYLAND_DISPLAY=kwin-wayland plasmashell || true
            kill "$KWIN_PID" 2>/dev/null || true
        '"'"
    elif command -v labwc &>/dev/null; then
        LAUNCH_DESC="labwc"
        LAUNCH_CMD="labwc"
    elif command -v kwin_wayland &>/dev/null; then
        LAUNCH_DESC="kwin_wayland (standalone)"
        LAUNCH_CMD="dbus-run-session kwin_wayland --xwayland --no-lockscreen"
    elif command -v weston &>/dev/null; then
        LAUNCH_DESC="weston"
        LAUNCH_CMD="weston --backend=wayland"
    else
        for candidate in foot kitty ghostty; do
            if command -v "$candidate" &>/dev/null; then
                LAUNCH_DESC="$candidate"
                LAUNCH_CMD="$candidate"
                break
            fi
        done
    fi

    if [[ -n "$LAUNCH_CMD" ]]; then
        echo "==> Auto-detected client: $LAUNCH_DESC  (override with --launch or LUMEN_LAUNCH)"
        LAUNCH_ARGS=(--launch "$LAUNCH_CMD")
    else
        echo "==> WARN: no supported client found (startplasma-wayland/kwin_wayland+plasmashell/labwc/weston/foot/kitty/ghostty)"
        echo "    Pass --launch <cmd> or set LUMEN_LAUNCH to specify a client"
    fi
fi

# ── Start lumen ────────────────────────────────────────────────────────────────
echo "==> Starting lumen  (Web UI: http://localhost:8080)"
echo "    Extra args: ${LAUNCH_ARGS[*]:-} $*"
exec "$LUMEN_BIN" "${LAUNCH_ARGS[@]}" "$@"

