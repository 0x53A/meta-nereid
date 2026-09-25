#!/bin/sh
set -e

TARGET=armv7-unknown-linux-gnueabihf

echo "Building for $TARGET..."
cargo build --release --target "$TARGET"

echo "Patching interpreter..."
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "target/$TARGET/release/bt-headphones"

echo "Binary ready at target/$TARGET/release/bt-headphones"
