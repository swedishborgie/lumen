# =============================================================================
# Stage 1 — Planner
#   Computes a cargo-chef recipe from the workspace manifests.
#   This stage is intentionally minimal — it only needs Rust + cargo-chef,
#   not the full C/C++ library stack.
# =============================================================================
FROM docker.io/ubuntu:25.10 AS planner

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --default-toolchain stable --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

# Pin with --locked for reproducibility; bump the version to pick up new releases.
RUN cargo install cargo-chef --locked --version 0.1.77

WORKDIR /build

# Copy the entire workspace so cargo-chef can resolve the full dependency graph.
COPY Cargo.toml Cargo.lock ./
COPY crates/lumen-compositor/Cargo.toml crates/lumen-compositor/
COPY crates/lumen-audio/Cargo.toml      crates/lumen-audio/
COPY crates/lumen-encode/Cargo.toml     crates/lumen-encode/
COPY crates/lumen-webrtc/Cargo.toml     crates/lumen-webrtc/
COPY crates/lumen-web/Cargo.toml        crates/lumen-web/
COPY crates/lumen-turn/Cargo.toml       crates/lumen-turn/
COPY crates/lumen-gamepad/Cargo.toml    crates/lumen-gamepad/
COPY src/ src/
COPY crates/ crates/

RUN cargo chef prepare --recipe-path recipe.json

# =============================================================================
# Stage 2 — Builder
#   1. Compiles all third-party dependencies from the recipe (cached layer).
#      This layer is only invalidated when Cargo.toml / Cargo.lock changes.
#   2. Compiles the application code on top (fast; re-runs on source changes).
# =============================================================================
FROM docker.io/ubuntu:25.10 AS builder

ENV DEBIAN_FRONTEND=noninteractive

# ── System build dependencies ─────────────────────────────────────────────────
RUN apt-get update && apt-get install -y --no-install-recommends \
    # Rust toolchain bootstrap
    curl \
    ca-certificates \
    # Build essentials
    build-essential \
    pkg-config \
    cmake \
    nasm \
    # Bindgen / LLVM
    clang \
    libclang-dev \
    llvm \
    # Wayland / Smithay
    libwayland-dev \
    libxkbcommon-dev \
    libpixman-1-dev \
    # EGL / GLES / GBM / DRM (GPU backend)
    libegl-dev \
    libgles2-mesa-dev \
    libgbm-dev \
    libdrm-dev \
    # libinput (input handling)
    libinput-dev \
    libevdev-dev \
    libudev-dev \
    # PulseAudio (lumen-audio)
    libpulse-dev \
    # PipeWire (lumen-audio)
    libpipewire-0.3-dev \
    libspa-0.2-dev \
    # Opus codec
    libopus-dev \
    # Software H.264 encoder
    libx264-dev \
    # VA-API hardware encoder
    libva-dev \
    # FFmpeg (ffmpeg-next / ffmpeg-sys-next crates)
    libavcodec-dev \
    libavformat-dev \
    libavutil-dev \
    libavfilter-dev \
    libavdevice-dev \
    libswscale-dev \
    libswresample-dev \
    # PAM (lumen-web authentication)
    libpam0g-dev \
    # OpenSSL (reqwest / openidconnect)
    libssl-dev \
    # Git (Cargo fetches Smithay from git)
    git \
    && rm -rf /var/lib/apt/lists/*

# ── Rust toolchain ─────────────────────────────────────────────────────────────
RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --default-toolchain stable --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

RUN cargo install cargo-chef --locked --version 0.1.77

WORKDIR /build

# ── Cache layer: compile all dependencies ─────────────────────────────────────
# This RUN step is the only one that changes when Cargo.toml / Cargo.lock
# changes. On source-only rebuilds the layer is reused and this step is
# skipped entirely, saving the bulk of compile time.
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# ── Full build ─────────────────────────────────────────────────────────────────
# All dependencies are already compiled above. Only application crates are
# recompiled here, which is fast.
COPY Cargo.toml Cargo.lock ./
COPY src/         src/
COPY crates/      crates/
COPY web/         web/

# Inject the release version so CARGO_PKG_VERSION is not the only source.
# In CI this is set to the stripped git tag (e.g. "0.2.0"); locally it is unset
# and build.rs falls back to the version in Cargo.toml.
ARG LUMEN_VERSION=
ENV LUMEN_VERSION=$LUMEN_VERSION

RUN cargo build --release --bin lumen
# Stage 2 — Runtime
#   Minimal Ubuntu desktop + XWayland + Firefox + lumen.
#   Select desktop environment at build time:
#     --build-arg DESKTOP=labwc   (default) — labwc lightweight compositor
#     --build-arg DESKTOP=kde     — KDE Plasma (kwin_wayland + plasmashell)
# =============================================================================
FROM docker.io/ubuntu:25.10 AS runtime

ENV DEBIAN_FRONTEND=noninteractive

# ── Enable multiverse (required for intel-media-va-driver-non-free) ───────────
RUN sed -i 's/^Components: main restricted$/Components: main restricted universe multiverse/' \
        /etc/apt/sources.list.d/ubuntu.sources

# ── Mozilla APT repo (Ubuntu 25.10's 'firefox' package is a snap stub) ────────
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg \
    && install -d -m 0755 /etc/apt/keyrings \
    && curl -fsSL https://packages.mozilla.org/apt/repo-signing-key.gpg \
        -o /etc/apt/keyrings/packages.mozilla.org.asc \
    && echo "deb [signed-by=/etc/apt/keyrings/packages.mozilla.org.asc] https://packages.mozilla.org/apt mozilla main" \
        > /etc/apt/sources.list.d/mozilla.list \
    && printf 'Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n' \
        > /etc/apt/preferences.d/mozilla \
    && rm -rf /var/lib/apt/lists/*

# ── Runtime packages (shared by all desktop variants) ────────────────────────
RUN apt-get update && apt-get install -y --no-install-recommends \
    # ── XWayland bridge ───────────────────────────────────────────────────────
    xwayland \
    # ── X11 test utilities ────────────────────────────────────────────────────
    x11-apps \
    # ── Browser ───────────────────────────────────────────────────────────────
    firefox \
    # ── Audio server ──────────────────────────────────────────────────────────
    pipewire-pulse \
    # ── GPU / Mesa runtime ────────────────────────────────────────────────────
    libgl1 \
    libglx-mesa0 \
    libgles2 \
    libgbm1 \
    libdrm2 \
    libdrm-amdgpu1 \
    libdrm-intel1 \
    libdrm-radeon1 \
    mesa-va-drivers \
    intel-media-va-driver-non-free \
    libva2 \
    libva-drm2 \
    # ── Video codec runtime ────────────────────────────────────────────────────
    libx264-164 \
    libopus0 \
    libavcodec61 \
    libavformat61 \
    libavutil59 \
    libavfilter10 \
    libavdevice61 \
    libswscale8 \
    libswresample5 \
    # ── Wayland / XKB ─────────────────────────────────────────────────────────
    libwayland-client0 \
    libwayland-server0 \
    libxkbcommon0 \
    # ── Input / udev ──────────────────────────────────────────────────────────
    libinput10 \
    libevdev2 \
    libudev1 \
    # ── PulseAudio client runtime (libpulse; pipewire-pulse is the server) ────
    libpulse0 \
    # ── PipeWire runtime (lumen-audio virtual sink) ───────────────────────────
    pipewire \
    wireplumber \
    libpipewire-0.3-0 \
    # ── KDE audio integration (volume applet QML module) ─────────────────────
    plasma-pa \
    # ── PAM runtime ───────────────────────────────────────────────────────────
    libpam0g \
    # ── Pixman (Smithay CPU renderer) ─────────────────────────────────────────
    libpixman-1-0 \
    # ── D-Bus (required by Firefox and desktop stack) ─────────────────────────
    dbus \
    dbus-x11 \
    # ── Fonts ─────────────────────────────────────────────────────────────────
    fonts-liberation \
    fonts-dejavu-core \
    # ── Utilities ─────────────────────────────────────────────────────────────
    iproute2 \
    && rm -rf /var/lib/apt/lists/*

# ── Desktop-environment-specific packages ────────────────────────────────────
# This ARG is declared AFTER the shared layer above so that changing DESKTOP
# does not bust the cache for the large shared apt-get step.
ENV LANG=C.UTF-8
ENV LC_ALL=C.UTF-8

ARG DESKTOP=labwc

# NOTE: no inline comments inside the apt-get lists — shell comments after a
# backslash-continued line (without a trailing backslash) break line joining,
# causing packages after the comment to be skipped.
RUN apt-get update && \
    if [ "$DESKTOP" = "kde" ]; then \
        apt-get install -y --no-install-recommends \
            kwin-wayland \
            plasma-desktop \
            plasma-workspace \
            konsole \
        ; \
    else \
        apt-get install -y --no-install-recommends \
            labwc \
            foot \
        ; \
    fi \
    && rm -rf /var/lib/apt/lists/*

# Expose the selected desktop to the entrypoint at runtime.
ENV DESKTOP=$DESKTOP

# ── Copy lumen binary from builder ────────────────────────────────────────────
COPY --from=builder /build/target/release/lumen /usr/local/bin/lumen

# ── Copy and prepare entrypoint ───────────────────────────────────────────────
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# ── Desktop-environment configuration ─────────────────────────────────────────
# labwc: autostart script (launches Firefox) + right-click menu definition.
# KDE:   kwinrc (disable screen locker, allow compositing in containers) +
#        autostart script (launches Firefox via plasma autostart mechanism).
RUN mkdir -p /root/.config/labwc
COPY docker/labwc-autostart /root/.config/labwc/autostart
RUN chmod +x /root/.config/labwc/autostart
COPY docker/labwc-menu.xml /root/.config/labwc/menu.xml

RUN mkdir -p /root/.config \
             /root/.config/autostart-scripts \
             /root/.local/share/konsole
COPY docker/kde-kwinrc /root/.config/kwinrc
COPY docker/kde-autostart /root/.config/autostart-scripts/lumen-kde-autostart.sh
RUN chmod +x /root/.config/autostart-scripts/lumen-kde-autostart.sh
# Pre-configure the Plasma taskbar (removes uninstalled systemsettings.desktop
# and org.kde.discover.desktop from the launcher; keeps only preferred://browser).
COPY docker/kde-plasma-desktop-appletsrc /root/.config/plasma-org.kde.plasma.desktop-appletsrc
# Pre-configure Konsole with a default Shell profile so it doesn't warn about
# a missing profile on first launch.
COPY docker/konsole-shell.profile /root/.local/share/konsole/Shell.profile
RUN kwriteconfig6 --file konsolerc --group "Desktop Entry" --key DefaultProfile Shell.profile
# Disable systemd-based Plasma session startup (default since Plasma 5.25).
# Containers have no systemd user instance, so the default causes startplasma-wayland
# to stall waiting for systemd unit activation. Per: https://wiki.archlinux.org/title/KDE#systemd_startup
RUN kwriteconfig6 --file startkderc --group General --key systemdBoot false

# kwin_wayland ships with cap_sys_nice=ep (effective+permitted) so it can set
# real-time thread priorities.  Container runtimes without CAP_SYS_NICE in the
# bounding set cause execve() to return EPERM, silently preventing kwin from
# launching.  Strip the file capability so kwin starts normally; it falls back
# to standard SCHED_OTHER scheduling.
RUN setcap -r /usr/bin/kwin_wayland

# ── Expose ports ──────────────────────────────────────────────────────────────
# 8080: lumen HTTP / WebRTC signaling
# 3478/udp: embedded TURN server control
# 50000-50010/udp: TURN relay data range
EXPOSE 8080
EXPOSE 3478/udp

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
