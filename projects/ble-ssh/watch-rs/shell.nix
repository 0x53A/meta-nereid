{ pkgs ? import <nixpkgs> {} }:

let
  # Cross-compilation toolchain for AsteroidOS (ARM)
  armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
  armCc = armPkgs.stdenv.cc;
  armPrefix = "armv7l-unknown-linux-gnueabihf";

  # ARM pkg-config search paths (bluer needs dbus)
  armPkgConfigPath = builtins.concatStringsSep ":" [
    "${armPkgs.dbus.dev}/lib/pkgconfig"
  ];

  # ARM library paths for the linker
  armLibDirs = builtins.concatStringsSep " " (map (p: "-L ${p}") [
    "${armPkgs.dbus.lib}/lib"
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
    # Native build deps
    pkg-config
    dbus.dev

    # ARM cross-compiler
    armCc

    # Deploy tools
    android-tools   # adb
    patchelf
  ];

  # Tell Cargo how to cross-compile for ARM
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${armCc}/bin/${armPrefix}-cc";
  CC_armv7_unknown_linux_gnueabihf = "${armCc}/bin/${armPrefix}-cc";

  # Point Rust's pkg-config crate at our ARM wrapper
  PKG_CONFIG_armv7_unknown_linux_gnueabihf = "${armPkgConfig}/bin/arm-pkg-config";
  PKG_CONFIG_ALLOW_CROSS = "1";

  # Pass ARM library search paths to the linker via rustflags
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS = "${armLibDirs}";

  shellHook = ''
    rustup target add armv7-unknown-linux-gnueabihf 2>/dev/null || true
    echo "ARM cross-compilation ready. Build with:"
    echo "  cargo build --release --target armv7-unknown-linux-gnueabihf"
  '';
}
