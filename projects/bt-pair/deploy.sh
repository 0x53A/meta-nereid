#!/bin/sh
# Deploy bt-pair to watch via ADB
# Usage: nix-shell --run ./deploy.sh

set -e

TARGET=armv7-unknown-linux-gnueabihf

echo "Building for $TARGET..."
cargo build --release --target "$TARGET"

echo "Patching interpreter..."
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "target/$TARGET/release/bt-pair"

echo "Pushing to watch..."
# Push binary via /tmp (direct push to /usr/lib can fail with ADB protocol errors)
adb push "target/$TARGET/release/bt-pair" /tmp/bt-pair
adb shell 'killall bt-pair 2>/dev/null; sleep 1; cp /tmp/bt-pair /usr/lib/bt-pair && chmod +x /usr/lib/bt-pair && rm /tmp/bt-pair'
adb push deploy/bt-pair.sh /usr/bin/bt-pair
adb shell chmod +x /usr/bin/bt-pair
adb push deploy/bt-pair.desktop /usr/share/applications/bt-pair.desktop

echo "Done! Tap the icon on your watch."
