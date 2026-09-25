#!/usr/bin/env bash
set -euo pipefail

PKG_NAME="hoki-podcast"
PKG_VERSION="0.1.0"
PKG_ARCH="armv7vehf-neon"
TARGET="armv7-unknown-linux-gnueabihf"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BINARY="$SCRIPT_DIR/target/$TARGET/release/$PKG_NAME"
OUT_DIR="$SCRIPT_DIR/target/package"
STAGING="$OUT_DIR/staging"

# Build if needed
if [ ! -f "$BINARY" ]; then
    echo "Binary not found. Building..."
    nix-shell --run "cargo build --release --target $TARGET"
fi

echo "Packaging $PKG_NAME $PKG_VERSION..."

rm -rf "$STAGING" "$OUT_DIR/$PKG_NAME"_*.ipk
mkdir -p "$STAGING"/{control,data/usr/lib,data/usr/bin,data/usr/share/applications}

# Patch and copy binary
cp "$BINARY" "$STAGING/data/usr/lib/$PKG_NAME"
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "$STAGING/data/usr/lib/$PKG_NAME"

# Launcher script
cp "$SCRIPT_DIR/deploy/$PKG_NAME.sh" "$STAGING/data/usr/bin/$PKG_NAME"
chmod 755 "$STAGING/data/usr/bin/$PKG_NAME"

# Desktop entry
cp "$SCRIPT_DIR/deploy/$PKG_NAME.desktop" "$STAGING/data/usr/share/applications/"

# debian-binary
echo "2.0" > "$STAGING/debian-binary"

# control file
cat > "$STAGING/control/control" <<EOF
Package: $PKG_NAME
Version: $PKG_VERSION
Architecture: $PKG_ARCH
Section: multimedia
Priority: optional
Maintainer: Lukas
Description: Podcast player for AsteroidOS
 Twig Audiobook podcast player with download and offline playback.
 Designed for the Fossil Gen 6 smartwatch.
EOF

# Build tarballs
(cd "$STAGING/control" && tar czf "$STAGING/control.tar.gz" .)
(cd "$STAGING/data" && tar czf "$STAGING/data.tar.gz" .)

# Build IPK (ar archive)
IPK="$OUT_DIR/${PKG_NAME}_${PKG_VERSION}_${PKG_ARCH}.ipk"
(cd "$STAGING" && ar r "$IPK" debian-binary control.tar.gz data.tar.gz 2>/dev/null)

# Cleanup staging
rm -rf "$STAGING"

echo "Package created: $IPK"
echo ""
echo "Install on watch:"
echo "  scp $IPK root@hoki.local:/tmp/"
echo "  ssh root@hoki.local opkg install /tmp/$(basename "$IPK")"
