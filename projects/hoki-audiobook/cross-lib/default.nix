{ pkgs ? import <nixpkgs> {} }:
let
  armCc = pkgs.pkgsCross.armv7l-hf-multiplatform.stdenv.cc;
in
pkgs.runCommand "hoki-audiobook-arm-link-stubs" {
  nativeBuildInputs = [ pkgs.python3 armCc ];
} ''
  python3 ${./generate.py} ${./manifest.json} ${./symbols} \
    ${armCc}/bin/armv7l-unknown-linux-gnueabihf-cc "$out"
''
