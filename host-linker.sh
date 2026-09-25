#!/bin/sh
# Each project's nix-shell supplies its native compiler and binutils.
exec cc "$@" -fuse-ld=bfd
