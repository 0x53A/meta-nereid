{ pkgs ? import <nixpkgs> {}, nativeOnly ? false }:

let
  armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
  armCc = armPkgs.stdenv.cc;
  armPrefix = "armv7l-unknown-linux-gnueabihf";
in
pkgs.mkShell {
  buildInputs = with pkgs; [
    pkg-config
    android-tools
    python3
  ] ++ pkgs.lib.optional (!nativeOnly) armCc;

  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER = if nativeOnly then "cc" else "${armCc}/bin/${armPrefix}-cc";
  CC_armv7_unknown_linux_gnueabihf = if nativeOnly then "cc" else "${armCc}/bin/${armPrefix}-cc";

  shellHook = ''
    ${pkgs.lib.optionalString (!nativeOnly) "rustup target add armv7-unknown-linux-gnueabihf 2>/dev/null || true"}
    echo "Build: cargo build --release --target armv7-unknown-linux-gnueabihf"
  '';
}
