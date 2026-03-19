# =============================================================================
# Stage 1 — Planner
#   Computes a cargo-chef recipe from the workspace manifests.
#   This stage is intentionally minimal — it only needs Rust + cargo-chef,
#   not the full C/C++ library stack.
# =============================================================================
FROM ubuntu:24.04 AS planner

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
FROM ubuntu:24.04 AS builder

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

RUN cargo build --release --bin lumen

# =============================================================================
# Stage 2 — Runtime
#   Minimal Ubuntu desktop: labwc + XWayland + Firefox + test apps + lumen.
# =============================================================================
FROM ubuntu:24.04 AS runtime

ENV DEBIAN_FRONTEND=noninteractive

# ── Enable multiverse (required for intel-media-va-driver-non-free) ───────────
RUN sed -i 's/^Components: main restricted$/Components: main restricted universe multiverse/' \
        /etc/apt/sources.list.d/ubuntu.sources

# ── Mozilla APT repo (Ubuntu 24.04's 'firefox' package is a snap stub) ────────
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

# ── Runtime packages ──────────────────────────────────────────────────────────
RUN apt-get update && apt-get install -y --no-install-recommends \
    # ── Wayland compositor (inner desktop) ────────────────────────────────────
    labwc \
    # ── XWayland bridge ───────────────────────────────────────────────────────
    xwayland \
    # ── X11 test utilities ────────────────────────────────────────────────────
    x11-apps \
    # ── Browser ───────────────────────────────────────────────────────────────
    firefox \
    # ── Terminal emulator ─────────────────────────────────────────────────────
    foot \
    # ── Audio server ──────────────────────────────────────────────────────────
    pulseaudio \
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
    libavcodec60 \
    libavformat60 \
    libavutil58 \
    libavfilter9 \
    libavdevice60 \
    libswscale7 \
    libswresample4 \
    # ── Wayland / XKB ─────────────────────────────────────────────────────────
    libwayland-client0 \
    libwayland-server0 \
    libxkbcommon0 \
    # ── Input / udev ──────────────────────────────────────────────────────────
    libinput10 \
    libevdev2 \
    libudev1 \
    # ── PulseAudio runtime ────────────────────────────────────────────────────
    libpulse0 \
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

# ── Copy lumen binary from builder ────────────────────────────────────────────
COPY --from=builder /build/target/release/lumen /usr/local/bin/lumen

# ── Copy static web assets served by the Axum HTTP server ────────────────────
COPY --from=builder /build/web /opt/lumen/web

# ── Copy and prepare entrypoint ───────────────────────────────────────────────
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# ── labwc default config: autostart Firefox on launch ─────────────────────────
RUN mkdir -p /root/.config/labwc
COPY docker/labwc-autostart /root/.config/labwc/autostart
RUN chmod +x /root/.config/labwc/autostart
COPY docker/labwc-menu.xml /root/.config/labwc/menu.xml

# ── Expose ports ──────────────────────────────────────────────────────────────
# 8080: lumen HTTP / WebRTC signaling
# 3478/udp: embedded TURN server control
# 50000-50010/udp: TURN relay data range
EXPOSE 8080
EXPOSE 3478/udp

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
