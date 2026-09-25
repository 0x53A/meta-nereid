{ pkgs ? import <nixpkgs> {} }:

let
  armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
  armCc = armPkgs.stdenv.cc;
  armPrefix = "armv7l-unknown-linux-gnueabihf";

  libPath = with pkgs; lib.makeLibraryPath [
    libGL
    libxkbcommon
    wayland
    vulkan-loader
    libpulseaudio
    fontconfig
  ];

  armPkgConfigPath = builtins.concatStringsSep ":" [
    "${toString ./.}/cross-pc"
    "${armPkgs.openssl.dev}/lib/pkgconfig"
    "${armPkgs.fontconfig.dev}/lib/pkgconfig"
    "${armPkgs.freetype.dev}/lib/pkgconfig"
    "${armPkgs.expat.dev}/lib/pkgconfig"
    "${armPkgs.wayland.dev}/lib/pkgconfig"
    "${armPkgs.libxkbcommon.dev}/lib/pkgconfig"
  ];

  armLibDirs = builtins.concatStringsSep " " (map (p: "-L ${p}") [
    "${toString ./.}/cross-lib"
    "${armPkgs.openssl.out}/lib"
    "${armPkgs.fontconfig.lib}/lib"
    "${armPkgs.freetype.out}/lib"
    "${armPkgs.expat.out}/lib"
    "${armPkgs.wayland.out}/lib"
    "${armPkgs.libxkbcommon.out}/lib"
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
    pkg-config
    openssl
    cacert
    libpulseaudio
    fontconfig
    libxkbcommon
    wayland
    libGL
    vulkan-loader
    vulkan-headers
    cmake
    armCc
    patchelf
  ];

  LD_LIBRARY_PATH = libPath;

  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${armCc}/bin/${armPrefix}-cc";
  CC_armv7_unknown_linux_gnueabihf = "${armCc}/bin/${armPrefix}-cc";
  PKG_CONFIG_armv7_unknown_linux_gnueabihf = "${armPkgConfig}/bin/arm-pkg-config";
  PKG_CONFIG_ALLOW_CROSS = "1";
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS = "${armLibDirs} -C link-arg=-Wl,--allow-shlib-undefined";

  shellHook = ''
    rustup target add armv7-unknown-linux-gnueabihf 2>/dev/null || true
    echo "ARM cross-compilation ready. Build with:"
    echo "  cargo build --release --target armv7-unknown-linux-gnueabihf"
  '';
}
