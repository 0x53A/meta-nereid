{
  # Pin to nixpkgs revision that cross-compiles cleanly (gnutls 3.8.12 breaks on newer)
  pkgs ? import (fetchTarball "https://github.com/NixOS/nixpkgs/archive/1267bb4920d0.tar.gz") {}
}:

let
  armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
  armCc = armPkgs.stdenv.cc;
  armPrefix = "armv7l-unknown-linux-gnueabihf";

  # Native libs for host builds
  libPath = with pkgs; lib.makeLibraryPath [
    libGL
    wayland
    libxkbcommon
    libinput
    udev
  ];

  # ARM pkg-config search paths
  armPkgConfigPath = builtins.concatStringsSep ":" [
    "${armPkgs.wayland.dev}/lib/pkgconfig"
    "${armPkgs.libxkbcommon.dev}/lib/pkgconfig"
    "${armPkgs.libinput.dev}/lib/pkgconfig"
    "${armPkgs.udev.dev}/lib/pkgconfig"
    "${armPkgs.libevdev}/lib/pkgconfig"
    "${armPkgs.mtdev}/lib/pkgconfig"
    "${armPkgs.fontconfig.dev}/lib/pkgconfig"
    "${armPkgs.freetype.dev}/lib/pkgconfig"
    "${armPkgs.expat.dev}/lib/pkgconfig"
  ];

  # ARM library paths for the linker
  armLibDirs = builtins.concatStringsSep " " (map (p: "-L ${p}") [
    "${armPkgs.wayland.out}/lib"
    "${armPkgs.libxkbcommon.out}/lib"
    "${armPkgs.libinput.out}/lib"
    "${armPkgs.udev.out}/lib"
    "${armPkgs.libevdev}/lib"
    "${armPkgs.mtdev}/lib"
    "${armPkgs.fontconfig.lib}/lib"
    "${armPkgs.freetype.out}/lib"
    "${armPkgs.expat.out}/lib"
  ]);

  armPkgConfig = pkgs.writeShellScriptBin "arm-pkg-config" ''
    export PKG_CONFIG_PATH="${armPkgConfigPath}"
    export PKG_CONFIG_LIBDIR="${armPkgConfigPath}"
    export PKG_CONFIG_SYSROOT_DIR=""
    exec ${pkgs.pkg-config}/bin/pkg-config "$@"
  '';
in
pkgs.mkShell {
  buildInputs = with pkgs; [
    # Native build deps (host check/test)
    pkg-config
    wayland
    libxkbcommon
    libinput
    udev
    libGL

    # ARM cross-compiler
    armCc

    # Deploy
    patchelf
    android-tools
  ];

  LD_LIBRARY_PATH = libPath;

  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${armCc}/bin/${armPrefix}-cc";
  CC_armv7_unknown_linux_gnueabihf = "${armCc}/bin/${armPrefix}-cc";

  PKG_CONFIG_armv7_unknown_linux_gnueabihf = "${armPkgConfig}/bin/arm-pkg-config";
  PKG_CONFIG_ALLOW_CROSS = "1";

  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS = "${armLibDirs}";

  shellHook = ''
    rustup target add armv7-unknown-linux-gnueabihf 2>/dev/null || true
    echo "Cross-compile with:"
    echo "  cargo build --release --target armv7-unknown-linux-gnueabihf"
  '';
}
