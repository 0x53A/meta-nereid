#!/usr/bin/env bash
set -euo pipefail
here=$(cd -- "$(dirname -- "$0")" && pwd)
layer=$(cd -- "$here/../.." && pwd)
mkdir -p "$here/build/bundle"
export CARGO_TARGET_DIR="$here/build/cargo"
cd "$layer/projects/hoki-lp-watchface"
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf --bin hoki-health-record --bin hoki-suspend-check'
for name in hoki-health-record hoki-suspend-check; do
    cp "$CARGO_TARGET_DIR/armv7-unknown-linux-gnueabihf/release/$name" "$here/build/bundle/$name"
done
export HOKI_PATCH_HELPER="$layer/patch-watch-elf.sh" HOKI_GESTURE_BUNDLE="$here/build/bundle"
nix-shell -p patchelf --run 'bash "$HOKI_PATCH_HELPER" "$HOKI_GESTURE_BUNDLE/hoki-health-record" "$HOKI_GESTURE_BUNDLE/hoki-suspend-check"'
cp "$here/probe.py" "$here/analyze.py" "$here/README.md" "$here/build/bundle/"
cd "$here/build/bundle"
sha256sum hoki-health-record hoki-suspend-check probe.py analyze.py README.md > SHA256SUMS
