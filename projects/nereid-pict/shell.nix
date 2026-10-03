# Native Rust watch daemon: no GUI, PipeWire, FFmpeg or system crypto libraries.
{ pkgs ? import (fetchTarball "https://github.com/NixOS/nixpkgs/archive/1267bb4920d0.tar.gz") {} }:
let
  cross = pkgs.pkgsCross.armv7l-hf-multiplatform;
  cc = cross.stdenv.cc;
in pkgs.mkShell {
  packages = [ cc pkgs.pkg-config pkgs.patchelf ];
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${cc}/bin/armv7l-unknown-linux-gnueabihf-cc";
  CC_armv7_unknown_linux_gnueabihf = "${cc}/bin/armv7l-unknown-linux-gnueabihf-cc";
  AR_armv7_unknown_linux_gnueabihf = "${cc.bintools}/bin/armv7l-unknown-linux-gnueabihf-ar";
}
