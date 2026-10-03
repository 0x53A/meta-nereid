{ pkgs ? import <nixpkgs> {}, nativeOnly ? false }:
let
  target = if nativeOnly then pkgs else pkgs.pkgsCross.armv7l-hf-multiplatform;
  base = import ../hoki-spo2/shell.nix { inherit pkgs nativeOnly; };
in base.overrideAttrs (old: {
  nativeBuildInputs = (old.nativeBuildInputs or [])
    ++ pkgs.lib.optional nativeOnly (pkgs.python3.withPackages (p: [ p.pillow ]));
  SODIUM_LIB_DIR = "${target.libsodium}/lib";
  SODIUM_SHARED = "1";
  LD_LIBRARY_PATH = (pkgs.lib.makeLibraryPath [
    pkgs.libsodium pkgs.libx11 pkgs.libxcursor pkgs.libxrandr pkgs.libxi
  ]) + ":" + (old.LD_LIBRARY_PATH or "");
})
