{ pkgs ? import <nixpkgs> {}, nativeOnly ? false }:
let
  target = if nativeOnly then pkgs else pkgs.pkgsCross.armv7l-hf-multiplatform;
  arm = pkgs.pkgsCross.armv7l-hf-multiplatform.stdenv.cc;
in pkgs.mkShell {
  packages = [ pkgs.dbus pkgs.pkg-config pkgs.patchelf pkgs.python3 ] ++ pkgs.lib.optional (!nativeOnly) arm;
  SODIUM_LIB_DIR = "${target.libsodium}/lib";
  SODIUM_SHARED = "1";
  LD_LIBRARY_PATH = "${pkgs.libsodium}/lib";
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${arm}/bin/armv7l-unknown-linux-gnueabihf-cc";
}
