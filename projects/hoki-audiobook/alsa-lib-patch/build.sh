#!/bin/bash
set -euo pipefail

# Cross-compile patched alsa-lib for armv7hf
# Patch: tolerate SYNC_PTR failure before hw_params (Qualcomm ASoC workaround)

BUILDDIR="$(cd "$(dirname "$0")" && pwd)"
SRCDIR="$BUILDDIR/alsa-lib-1.2.13"
PREFIX="$BUILDDIR/install"

cd "$SRCDIR"

echo "==> Configuring alsa-lib 1.2.13 for ARM cross-compile..."
./configure \
    --host=armv7l-unknown-linux-gnueabihf \
    --prefix="$PREFIX" \
    --disable-static \
    --disable-python \
    --disable-topology \
    --without-debug \
    CC=armv7l-unknown-linux-gnueabihf-cc \
    2>&1 | tail -5

echo "==> Building..."
make -j$(nproc) 2>&1 | tail -5

echo "==> Installing to $PREFIX..."
make install 2>&1 | tail -3

echo ""
echo "==> Built:"
ls -la "$PREFIX/lib/libasound.so"*
file "$PREFIX/lib/libasound.so.2.0.0"

echo ""
echo "==> Patching rpath..."
patchelf --set-rpath /usr/lib:/lib "$PREFIX/lib/libasound.so.2.0.0"

echo ""
echo "Deploy to watch:"
echo "  scp $PREFIX/lib/libasound.so.2.0.0 root@hoki.local:/usr/lib/libasound.so.2.0.0"
echo "  ssh root@hoki.local 'killall pulseaudio 2>/dev/null; sleep 2; ls -la /usr/lib/libasound*'"
