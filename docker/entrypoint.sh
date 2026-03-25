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
# /run/user/<uid> is pre-created in the image (see Dockerfile).
export XDG_RUNTIME_DIR="/run/user/$(id -u)"

echo "==> XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"

# ── D-Bus ──────────────────────────────────────────────────────────────────────
if command -v dbus-daemon &>/dev/null && [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]]; then
    eval "$(dbus-launch --sh-syntax)"
    echo "==> D-Bus session started: $DBUS_SESSION_BUS_ADDRESS"
fi

# ── /tmp/.X11-unix ────────────────────────────────────────────────────────────
# XWayland (used by kwin_wayland and other compositors) requires this directory
# to create its UNIX-domain socket.  It is not created automatically in a
# container because there is no X server running.
mkdir -p /tmp/.X11-unix
chmod 1777 /tmp/.X11-unix

# ── PipeWire ──────────────────────────────────────────────────────────────────
# lumen-audio creates a PipeWire virtual sink for audio capture.  The PipeWire
# daemon must be running before lumen starts, otherwise the sink creation fails.
#
# pipewire-pulse is used instead of the standalone PulseAudio daemon so that
# all audio clients (including KDE apps using libpulse) land in the same
# PipeWire graph and can be routed to lumen's virtual sink.
if command -v pipewire &>/dev/null; then
    pipewire &
    PIPEWIRE_PID=$!
    # Give pipewire a moment to initialise before wireplumber connects to it.
    sleep 0.5
    if command -v wireplumber &>/dev/null; then
        wireplumber &
    fi
    # Start the PulseAudio-compatible server so libpulse clients (Firefox,
    # KDE plasmoids, etc.) connect to PipeWire rather than a separate daemon.
    if command -v pipewire-pulse &>/dev/null; then
        pipewire-pulse &
    fi
    echo "==> PipeWire started (pid $PIPEWIRE_PID)"
else
    echo "==> WARN: pipewire not found — audio capture will be unavailable"
fi

# ── uinput (virtual gamepad devices) ─────────────────────────────────────────
if [[ -c /dev/uinput ]]; then
    echo "==> /dev/uinput available — gamepad support enabled"
else
    echo "==> WARN: /dev/uinput not found — gamepad support disabled"
    echo "    Pass --device /dev/uinput --group-add \$(stat -c '%g' /dev/uinput) to enable gamepad forwarding"
fi

# ── Start lumen ────────────────────────────────────────────────────────────────
# DRI node detection, TURN IP detection, clipboard bridge setup, and desktop
# launch are all handled internally by lumen.
# DESKTOP is set at image build time via ARG/ENV; it can be overridden at
# container run time with -e DESKTOP=kde|labwc.
DESKTOP="${DESKTOP:-labwc}"
echo "==> Starting lumen  (Web UI: http://localhost:8080, desktop: $DESKTOP)"

if [ "$DESKTOP" = "kde" ]; then
    echo "    Args: --auth none --desktop kde $*"
    # Enable Qt/KDE debug logging so kwin and plasma_session startup failures
    # appear in the container log.  Remove these once KDE is stable.
    # export QT_LOGGING_RULES="kwin*=true;org.kde.plasma.session=true;kf.*=true;org.kde.kcminit=true;qt.dbus=true;qt.qml.*=true;qt.scenegraph.*=true"
    # export QT_MESSAGE_PATTERN="[%{category}] %{message}"
    # export QT_DEBUG_PLUGINS=1
    # QSG_INFO=1 prints the OpenGL renderer/driver ksplashqml picks at startup;
    # if it crashes before rendering this will be the last line before the abort.
    # export QSG_INFO=1
    # Force the software renderer so ksplashqml doesn't crash if GPU GL is broken.
    # Remove once we confirm whether ksplash works with the GPU path.
    # export QSG_RENDER_LOOP=basic
    # export WAYLAND_DEBUG=1
    exec "$LUMEN_BIN" \
        --auth none \
        --desktop kde \
        "$@"
else
    echo "    Args: --auth none --launch labwc $*"
    exec "$LUMEN_BIN" \
        --auth none \
        --launch labwc \
        "$@"
fi

