#!/bin/sh
# Build guest WASM + host binary
# Usage: nix-shell --run ./build.sh [--desktop]
set -e

DESKTOP="${1:-}"
GUEST_DIR="$(dirname "$0")/../hoki-wasm-guest"
HOST_DIR="$(dirname "$0")"

echo "=== Building WASM guest ==="
cargo build --release --manifest-path "$GUEST_DIR/Cargo.toml" --target wasm32-unknown-unknown

# Copy guest.wasm where the host expects it (include_bytes!)
cp "$GUEST_DIR/target/wasm32-unknown-unknown/release/hoki_wasm_guest.wasm" "$HOST_DIR/guest.wasm"

WASM_SIZE=$(du -h "$HOST_DIR/guest.wasm" | cut -f1)
echo "Guest WASM: $WASM_SIZE"

if [ "$DESKTOP" = "--desktop" ]; then
    echo "=== Building host (desktop) ==="
    cargo build --release --manifest-path "$HOST_DIR/Cargo.toml"
    echo "Run: $HOST_DIR/target/release/hoki-wasm-host"
else
    echo "=== Building host (ARM) ==="
    cargo build --release --manifest-path "$HOST_DIR/Cargo.toml" --target armv7-unknown-linux-gnueabihf

    echo "=== Patching ==="
    patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
             --set-rpath /usr/lib:/lib \
             "$HOST_DIR/target/armv7-unknown-linux-gnueabihf/release/hoki-wasm-host"

    echo "Done. Deploy with: ./deploy.sh"
fi
