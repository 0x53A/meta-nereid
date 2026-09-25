#!/usr/bin/env bash
# Verify the watch loader contract; rewrite ELF metadata only when it differs.
set -euo pipefail
if [ "$#" -eq 0 ]; then
    echo "usage: patch-watch-elf.sh ELF..." >&2
    exit 2
fi
for binary in "$@"; do
    args=()
    interpreter=$(patchelf --print-interpreter "$binary")
    rpath=$(patchelf --print-rpath "$binary")
    if [ "$interpreter" != /lib/ld-linux-armhf.so.3 ]; then
        args+=(--set-interpreter /lib/ld-linux-armhf.so.3)
    fi
    if [ "$rpath" != /usr/lib:/lib ]; then
        args+=(--set-rpath /usr/lib:/lib)
    fi
    if [ "${#args[@]}" -gt 0 ]; then
        patchelf "${args[@]}" "$binary"
    fi
    [ "$(patchelf --print-interpreter "$binary")" = /lib/ld-linux-armhf.so.3 ]
    [ "$(patchelf --print-rpath "$binary")" = /usr/lib:/lib ]
done
