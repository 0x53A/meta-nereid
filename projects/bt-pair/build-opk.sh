#!/usr/bin/env bash
# Build bt-pair and package as .opk for AsteroidOS
# Usage: nix-shell --run ./build-opk.sh
set -euo pipefail

VERSION="0.1.0"
TARGET=armv7-unknown-linux-gnueabihf
BINARY="target/$TARGET/release/bt-pair"
OPK_NAME="bt-pair_${VERSION}_armv7vehf-neon.opk"

echo "==> Building for $TARGET..."
cargo build --release --target "$TARGET"

echo "==> Patching binary..."
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "$BINARY"

echo "==> Assembling opk..."
PKG_DIR=$(mktemp -d)
trap "rm -rf $PKG_DIR" EXIT

# --- data.tar.gz ---
DATA_DIR="$PKG_DIR/data"
mkdir -p "$DATA_DIR/usr/lib"
mkdir -p "$DATA_DIR/usr/bin"
mkdir -p "$DATA_DIR/usr/share/applications"

cp "$BINARY" "$DATA_DIR/usr/lib/bt-pair"
cp deploy/bt-pair.sh "$DATA_DIR/usr/bin/bt-pair"
chmod +x "$DATA_DIR/usr/bin/bt-pair"
cp deploy/bt-pair.desktop "$DATA_DIR/usr/share/applications/"

(cd "$DATA_DIR" && tar czf "$PKG_DIR/data.tar.gz" .)

# --- control.tar.gz ---
CTRL_DIR="$PKG_DIR/control"
mkdir -p "$CTRL_DIR"

INSTALLED_SIZE=$(du -sk "$DATA_DIR" | cut -f1)

cat > "$CTRL_DIR/control" <<EOF
Package: bt-pair
Version: ${VERSION}
Description: Bluetooth headphone manager for AsteroidOS
 Scan, pair, connect, and manage Bluetooth audio devices
 from your watch.
Section: base/utils
Priority: optional
Maintainer: Lukas Rieger
License: MIT
Architecture: armv7vehf-neon
OE: bt-pair
Depends: libc6, bluez5
Installed-Size: ${INSTALLED_SIZE}
EOF

(cd "$CTRL_DIR" && tar czf "$PKG_DIR/control.tar.gz" .)

# --- assemble .opk ---
echo "2.0" > "$PKG_DIR/debian-binary"

mkdir -p binaries
(cd "$PKG_DIR" && ar rc "$OLDPWD/binaries/$OPK_NAME" debian-binary control.tar.gz data.tar.gz)

echo "==> Built: binaries/$OPK_NAME ($(du -h "binaries/$OPK_NAME" | cut -f1))"
echo "    Install: adb push binaries/$OPK_NAME /tmp/ && adb shell opkg install /tmp/$OPK_NAME"
