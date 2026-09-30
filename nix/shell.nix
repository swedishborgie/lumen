{ pkgs }:

# `nix develop` build environment. Replaces the per-distro prerequisite lists
# in the documentation with a single reproducible shell.
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    # Rust toolchain
    cargo
    rustc
    rustfmt
    clippy
    rust-analyzer
    # Native build tooling
    pkg-config
    cmake
    nasm
    clang
    llvmPackages.libclang
    # Cargo fetches smithay from git
    git

    # Lumen's native library dependencies
    ffmpeg
    x264
    linux-pam
    pipewire
    libva
    libdrm
    libgbm
    libinput
    libevdev
    systemd
    wayland
    wayland-protocols
    libxkbcommon
    pixman
    openssl
  ];

  # bindgen (ffmpeg-sys-next, pam-sys) needs to locate libclang.
  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
  BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.glibc.dev}/include";

  # Convenience for running a locally built binary against the shell's libs.
  shellHook = ''
    echo "Lumen dev shell — build with: cargo build --release"
  '';
}
