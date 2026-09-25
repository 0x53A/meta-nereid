{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = with pkgs; [ pkg-config openssl cacert libpulseaudio fontconfig freetype expat wayland libxkbcommon ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ openssl libpulseaudio fontconfig freetype wayland libxkbcommon ]);
}
