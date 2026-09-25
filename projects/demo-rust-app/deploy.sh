#!/bin/sh
# Deploy demo-asteroid-app to watch via ADB
# Usage: nix-shell --run ./deploy.sh

set -e

TARGET=armv7-unknown-linux-gnueabihf

echo "Building for $TARGET..."
cargo build --release --target "$TARGET"

echo "Patching interpreter..."
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "target/$TARGET/release/demo-asteroid-app"

echo "Pushing to watch..."
# Push binary via /tmp (direct push to /usr/lib can fail with ADB protocol errors)
adb push "target/$TARGET/release/demo-asteroid-app" /tmp/demo-asteroid-app
adb shell 'killall demo-asteroid-app 2>/dev/null; sleep 1; cp /tmp/demo-asteroid-app /usr/lib/demo-asteroid-app && chmod +x /usr/lib/demo-asteroid-app && rm /tmp/demo-asteroid-app'
adb push deploy/demo-asteroid-app.sh /usr/bin/demo-asteroid-app
adb shell chmod +x /usr/bin/demo-asteroid-app
adb push deploy/demo-asteroid-app.desktop /usr/share/applications/demo-asteroid-app.desktop

echo "Done! Tap the icon on your watch."
