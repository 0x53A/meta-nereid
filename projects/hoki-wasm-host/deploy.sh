#!/bin/sh
# Deploy to watch via SSH (WiFi)
set -e

TARGET=armv7-unknown-linux-gnueabihf
HOST=root@hoki.local
BIN="target/$TARGET/release/hoki-wasm-host"

if [ ! -f "$BIN" ]; then
    echo "Build first: nix-shell --run ./build.sh"
    exit 1
fi

echo "Deploying to watch..."
scp "$BIN" "$HOST:/tmp/hoki-wasm-host"
ssh "$HOST" 'killall hoki-wasm-host 2>/dev/null || true; sleep 1; cp /tmp/hoki-wasm-host /usr/lib/hoki-wasm-host && chmod +x /usr/lib/hoki-wasm-host && rm /tmp/hoki-wasm-host'
scp deploy/hoki-wasm-host.sh "$HOST:/usr/bin/hoki-wasm-host"
ssh "$HOST" 'chmod +x /usr/bin/hoki-wasm-host'
scp deploy/hoki-wasm-host.desktop "$HOST:/usr/share/applications/"

echo "Done! Tap the icon on your watch."
