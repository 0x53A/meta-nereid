{ pkgs ? import <nixpkgs> {} }:
let
  cross = pkgs.pkgsCross.armv7l-hf-multiplatform.stdenv.cc;
  prefix = "armv7l-unknown-linux-gnueabihf";
in pkgs.mkShell {
  packages = [ cross pkgs.perl pkgs.gnumake pkgs.pkg-config pkgs.patchelf ];
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${cross}/bin/${prefix}-cc";
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = "${../../host-linker.sh}";
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS = "-C link-arg=-fuse-ld=bfd";
  CC_armv7_unknown_linux_gnueabihf = "${cross}/bin/${prefix}-cc";
  AR_armv7_unknown_linux_gnueabihf = "${cross.bintools}/bin/${prefix}-ar";
}
