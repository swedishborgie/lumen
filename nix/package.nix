# Lumen — Nix package definition.
#
# Generic installer build: no host-specific configuration lives here. Consume
# it via `pkgs.callPackage ./nix/package.nix { }`, the flake's
# `packages.<system>.lumen`, or the `services.lumen` NixOS module.
{
  lib,
  rustPlatform,
  pkg-config,
  cmake,
  nasm,
  clang,
  # Pinned to FFmpeg 7: FFmpeg 8 requires a non-NULL hw_frames_ctx on the
  # buffer source filter, which breaks Lumen's DMA-BUF -> VA-API zero-copy
  # pipeline ("avfilter_graph_create_filter (buffer) failed: -22"). The
  # distro packages build against FFmpeg <=7 for the same reason.
  ffmpeg_7,
  x264,
  linux-pam,
  pipewire,
  libva,
  libdrm,
  libgbm,
  libinput,
  libevdev,
  systemd,
  wayland,
  wayland-protocols,
  libxkbcommon,
  pixman,
  openssl,
  version ? "0.2.1",
  # Opt-in NVIDIA NVENC hardware encoding (off by default, matching upstream).
  nvenc ? false,
}:

let
  # Only keep files that can affect the compiled artifact. Excluding docs,
  # packaging metadata and VCS noise keeps the source hash stable across
  # documentation changes and avoids copying a local `target/` directory.
  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      let
        base = baseNameOf path;
      in
      !(builtins.elem base [
        ".git"
        ".github"
        ".idea"
        "target"
        "docs"
        "dev-docs"
        "docker"
      ])
      && !(type == "file" && lib.hasSuffix ".md" base)
      && base != "screenshot.png";
  };
in
rustPlatform.buildRustPackage {
  pname = "lumen";
  inherit version src;

  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes = {
      # smithay is pinned to a git revision in Cargo.lock, so Nix needs the
      # hash of the fetched tree. Run `nix build` once with `lib.fakeHash`
      # and substitute the hash Nix reports.
      "smithay-0.7.0" = "sha256-GMKOTa0yqYXo5nOpsjYJESUnOobAVh0TW7md2CMezIE=";
    };
  };

  # build.rs injects this into the binary (`lumen --version`).
  env.LUMEN_VERSION = version;

  nativeBuildInputs = [
    pkg-config
    cmake
    nasm # aws-lc-sys (str0m's WebRTC crypto backend)
    clang
    rustPlatform.bindgenHook # ffmpeg-sys-next / pam-sys need libclang
  ];

  buildInputs = [
    ffmpeg_7
    x264
    linux-pam
    pipewire
    libva
    libdrm
    libgbm
    libinput
    libevdev
    systemd # libudev
    wayland
    wayland-protocols
    libxkbcommon
    pixman
    openssl
  ];

  cargoBuildFlags = lib.optionals nvenc [ "--features" "nvenc" ];

  # The test suite spins up a full Wayland/GPU stack that is not available in
  # the Nix sandbox; build-only is sufficient for packaging.
  doCheck = false;

  # Ship the non-NixOS integration files for parity with the .deb/.rpm. The
  # NixOS module generates its own systemd unit and udev handling.
  postInstall = ''
    install -Dm644 pkgs/example.env $out/share/lumen/example.env
    install -Dm644 pkgs/lumen@.service $out/lib/systemd/system/lumen@.service
    install -Dm644 pkgs/70-lumen-uinput.rules \
      $out/lib/udev/rules.d/70-lumen-uinput.rules
  '';

  meta = {
    description = "Wayland compositor that streams the desktop to browsers via WebRTC";
    homepage = "https://github.com/swedishborgie/lumen";
    license = lib.licenses.mit;
    mainProgram = "lumen";
    platforms = lib.platforms.linux;
  };
}
