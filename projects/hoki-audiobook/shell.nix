{ pkgs ? import <nixpkgs> {} }:

let
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
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-good
    gst_all_1.gst-plugins-bad
  ];

  # Stub .pc files for GStreamer/glib cross-compilation.
  # The watch already has GStreamer 1.24.13 + glib installed; we only need
  # pkg-config to report the right version and link flags so that
  # gstreamer-sys / glib-sys build scripts succeed.  No actual cross-compiled
  # libraries are needed at build time — the final binary links at runtime
  # against the watch's /usr/lib.
  crossPcDir = ./cross-pc;
  crossLibDir = import ./cross-lib { inherit pkgs; };

  # ARM pkg-config search paths (standard libs + GStreamer/glib stubs)
  armPkgConfigPath = builtins.concatStringsSep ":" [
    "${armPkgs.fontconfig.dev}/lib/pkgconfig"
    "${armPkgs.freetype.dev}/lib/pkgconfig"
    "${armPkgs.expat.dev}/lib/pkgconfig"
    "${armPkgs.wayland.dev}/lib/pkgconfig"
    "${armPkgs.libxkbcommon.dev}/lib/pkgconfig"
    "${crossPcDir}"
  ];

  # ARM stub libs for GStreamer/glib go FIRST so the linker finds them
  # before the native x86_64 libs that NIX_LDFLAGS injects.
  # Also pass --allow-shlib-undefined so the linker doesn't error on
  # symbols from our stub .so files (they'll be resolved at runtime on the watch).
  armLibDirs = builtins.concatStringsSep " " (
    (map (p: "-L ${p}") [
      "${crossLibDir}"
      "${armPkgs.fontconfig.lib}/lib"
      "${armPkgs.freetype.out}/lib"
      "${armPkgs.expat.out}/lib"
      "${armPkgs.wayland.out}/lib"
      "${armPkgs.libxkbcommon.out}/lib"
    ])
  );

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
    fontconfig
    libxkbcommon
    wayland
    libGL
    vulkan-loader
    vulkan-headers
    cmake

    # GStreamer (native, for desktop preview)
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-good
    gst_all_1.gst-plugins-bad   # for opusparse etc.

    # ARM cross-compiler
    armCc

    # Deploy tools
    patchelf
    android-tools

    # ARM pkg-config wrapper (for manual testing)
    armPkgConfig
  ];

  LD_LIBRARY_PATH = libPath;

  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${armCc}/bin/${armPrefix}-cc";
  CC_armv7_unknown_linux_gnueabihf = "${armCc}/bin/${armPrefix}-cc";

  PKG_CONFIG_armv7_unknown_linux_gnueabihf = "${armPkgConfig}/bin/arm-pkg-config";
  PKG_CONFIG_ALLOW_CROSS = "1";

  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS = "${armLibDirs}";

  shellHook = ''
    rustup target add armv7-unknown-linux-gnueabihf 2>/dev/null || true
    echo "Audiobook app dev shell ready."
    echo "  Desktop:  cargo run"
    echo "  Watch:    cargo build --release --target armv7-unknown-linux-gnueabihf"
  '';
}
