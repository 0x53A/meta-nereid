#!/usr/bin/env bash
# Build from the shared Pict engine without desktop/FFmpeg/PipeWire dependencies.
set -euo pipefail
: "${PICT_WATCH_BUILD:?Set the Hoki build root}"
: "${PICT_WATCH_ELF_PATCH:?Set the watch ELF patch helper path}"
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="$PICT_WATCH_BUILD/pict-watch/rust"
mkdir -p "$PICT_WATCH_BUILD/pict-watch"
nix-shell --run 'cargo +1.97.1 build --locked --release --target armv7-unknown-linux-gnueabihf --bin nereid-pict'
nix-shell -p patchelf --run 'bash "$PICT_WATCH_ELF_PATCH" "$CARGO_TARGET_DIR/armv7-unknown-linux-gnueabihf/release/nereid-pict"'
cp "$CARGO_TARGET_DIR/armv7-unknown-linux-gnueabihf/release/nereid-pict" "$PICT_WATCH_BUILD/pict-watch/nereid-pict"
