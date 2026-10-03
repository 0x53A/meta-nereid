{ pkgs ? import <nixpkgs> {}, nativeOnly ? false }:
import ../hoki-spo2/shell.nix { inherit pkgs nativeOnly; }
