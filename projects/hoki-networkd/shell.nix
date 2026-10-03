{ pkgs ? import <nixpkgs> {} }:
let
  armCc = pkgs.pkgsCross.armv7l-hf-multiplatform.stdenv.cc;
  armPrefix = "armv7l-unknown-linux-gnueabihf";
in pkgs.mkShell {
  # The availability publisher needs only Rust/libc and the ARM cross compiler.
  buildInputs = [ armCc ];
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${armCc}/bin/${armPrefix}-cc";
  CC_armv7_unknown_linux_gnueabihf = "${armCc}/bin/${armPrefix}-cc";
  shellHook = ''
    rustup target add armv7-unknown-linux-gnueabihf
  '';
}
