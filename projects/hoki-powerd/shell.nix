{ pkgs ? import <nixpkgs> {} }:

let
  armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
  armCc = armPkgs.stdenv.cc;
  armPrefix = "armv7l-unknown-linux-gnueabihf";
in
pkgs.mkShell {
  buildInputs = with pkgs; [
    pkg-config
    armCc
    android-tools
  ];

  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${armCc}/bin/${armPrefix}-cc";
  CC_armv7_unknown_linux_gnueabihf = "${armCc}/bin/${armPrefix}-cc";

  shellHook = ''
    rustup target add armv7-unknown-linux-gnueabihf 2>/dev/null || true
    echo "Build: cargo build --release --target armv7-unknown-linux-gnueabihf"
  '';
}
