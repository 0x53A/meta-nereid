{ nativeOnly ? false }:
let
  pkgs = import (fetchTarball "https://github.com/NixOS/nixpkgs/archive/1267bb4920d0.tar.gz") {};
  base = import ../nereid-compositor/shell.nix { inherit nativeOnly pkgs; };
in base.overrideAttrs (old: {
  buildInputs = old.buildInputs ++ [ pkgs.fontconfig pkgs.freetype ];
})
