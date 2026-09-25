{ pkgs ? import <nixpkgs> {} }:

let
  # Cross-compilation toolchain for AsteroidOS (ARM)
  armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
  armCc = armPkgs.stdenv.cc;
  armPrefix = "armv7l-unknown-linux-gnueabihf";

  # Native libs for desktop preview
  libPath = with pkgs; lib.makeLibraryPath [
    libGL
    libxkbcommon
    wayland
    vulkan-loader
    fontconfig
  ];

  # ARM pkg-config search paths
  armPkgConfigPath = builtins.concatStringsSep ":" [
    "${armPkgs.fontconfig.dev}/lib/pkgconfig"
    "${armPkgs.freetype.dev}/lib/pkgconfig"
    "${armPkgs.expat.dev}/lib/pkgconfig"
    "${armPkgs.wayland.dev}/lib/pkgconfig"
    "${armPkgs.libxkbcommon.dev}/lib/pkgconfig"
  ];

  # ARM library paths for the linker
  armLibDirs = builtins.concatStringsSep " " (map (p: "-L ${p}") [
    "${armPkgs.fontconfig.lib}/lib"
    "${armPkgs.freetype.out}/lib"
    "${armPkgs.expat.out}/lib"
    "${armPkgs.wayland.out}/lib"
    "${armPkgs.libxkbcommon.out}/lib"
  ]);

  # A pkg-config wrapper that searches ARM library paths only
  armPkgConfig = pkgs.writeShellScriptBin "arm-pkg-config" ''
    export PKG_CONFIG_PATH="${armPkgConfigPath}"
    export PKG_CONFIG_LIBDIR="${armPkgConfigPath}"
    export PKG_CONFIG_SYSROOT_DIR=""
    exec ${pkgs.pkg-config}/bin/pkg-config "$@"
  '';
in
pkgs.mkShell {
  buildInputs = with pkgs; [
    # Native build deps (desktop preview)
    pkg-config
    fontconfig
    libxkbcommon
    wayland
    libGL
    vulkan-loader
    vulkan-headers
    cmake

    # ARM cross-compiler
    armCc

    # Deploy tool
    android-tools   # adb
  ];

  LD_LIBRARY_PATH = libPath;

  # Tell Cargo how to cross-compile for ARM
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${armCc}/bin/${armPrefix}-cc";
  CC_armv7_unknown_linux_gnueabihf = "${armCc}/bin/${armPrefix}-cc";

  # Point Rust's pkg-config crate at our ARM wrapper for finding .pc files
  PKG_CONFIG_armv7_unknown_linux_gnueabihf = "${armPkgConfig}/bin/arm-pkg-config";
  PKG_CONFIG_ALLOW_CROSS = "1";

  # Pass ARM library search paths to the linker via rustflags
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS = "${armLibDirs}";

  shellHook = ''
    # Ensure the ARM Rust target is installed
    rustup target add armv7-unknown-linux-gnueabihf 2>/dev/null || true
    echo "ARM cross-compilation ready. Build with:"
    echo "  cargo build --release --target armv7-unknown-linux-gnueabihf"
  '';
}
