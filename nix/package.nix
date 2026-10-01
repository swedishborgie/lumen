# Lumen — Nix package definition (crane).
#
# Split into two derivations so iteration is fast:
#
#   * `cargoArtifacts` (buildDepsOnly) is keyed on Cargo.lock and the native
#     build inputs, not on the workspace source. Every crates.io/git dependency
#     is compiled exactly once and reused until Cargo.lock changes.
#   * `buildPackage` then compiles only the Lumen workspace crates on top.
#
# crane also resolves git dependencies (smithay) directly from Cargo.lock, so
# no outputHashes are needed.
{
  lib,
  craneLib,
  rustPlatform,
  pkg-config,
  cmake,
  nasm,
  clang,
  ffmpeg,
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
  # Keep Rust sources, the embedded web assets (rust-embed) and the packaging
  # files run in postInstall. Excluding docs, flake files and `nix/` means
  # editing the flake or documentation does not invalidate the workspace build.
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
        "nix"
        "flake.nix"
        "flake.lock"
      ])
      && !(type == "file" && lib.hasSuffix ".md" base)
      && base != "screenshot.png";
  };

  commonArgs = {
    pname = "lumen";
    inherit version src;
    strictDeps = true;

    # build.rs injects this into the binary (`lumen --version`).
    LUMEN_VERSION = version;

    nativeBuildInputs = [
      pkg-config
      cmake
      nasm # aws-lc-sys (str0m's WebRTC crypto backend)
      clang
      rustPlatform.bindgenHook # ffmpeg-sys-next / pam-sys need libclang
    ];

    buildInputs = [
      ffmpeg
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

    cargoBuildExtraArgs = lib.optionalString nvenc "--features nvenc";
  };

  # Compile every dependency once; reused by buildPackage below and cacheable
  # on its own (this is the layer to push to Cachix/attic in CI).
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;

    # The test suite spins up a full Wayland/GPU stack unavailable in the sandbox.
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
)
