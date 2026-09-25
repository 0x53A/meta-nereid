#!/bin/bash
set -euo pipefail

PKG="0x53a.egui-demo"
VERSION="0.1.0"
ARCH="armv7vehf-neon"
SRCDIR="$(cd "$(dirname "$0")" && pwd)"
PKG_DIR=$(mktemp -d)
trap "rm -rf $PKG_DIR" EXIT

BINARY="$SRCDIR/target/armv7-unknown-linux-gnueabihf/release/hoki-egui-demo"

if [ ! -f "$BINARY" ]; then
    echo "ERROR: No ARM binary found at $BINARY"
    echo "Build first:"
    echo "  cd $SRCDIR && nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'"
    exit 1
fi

echo "==> Packaging hoki-egui-demo..."

# --- data.tar.gz ---
DATA_DIR="$PKG_DIR/data"
mkdir -p "$DATA_DIR/usr/lib"
mkdir -p "$DATA_DIR/usr/bin"
mkdir -p "$DATA_DIR/usr/share/applications"

cp "$BINARY" "$DATA_DIR/usr/lib/hoki-egui-demo"
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 --set-rpath /usr/lib:/lib "$DATA_DIR/usr/lib/hoki-egui-demo"
cp "$SRCDIR/deploy/hoki-egui-demo" "$DATA_DIR/usr/bin/hoki-egui-demo"
chmod 755 "$DATA_DIR/usr/bin/hoki-egui-demo"
cp "$SRCDIR/deploy/hoki-egui-demo.desktop" "$DATA_DIR/usr/share/applications/"

(cd "$DATA_DIR" && tar czf "$PKG_DIR/data.tar.gz" .)

# --- control.tar.gz ---
CTRL_DIR="$PKG_DIR/control"
mkdir -p "$CTRL_DIR"

INSTALLED_SIZE=$(du -sk "$DATA_DIR" | cut -f1)

cat > "$CTRL_DIR/control" <<EOF
Package: ${PKG}
Version: ${VERSION}-r0
Description: egui demo app (software-rendered)
Section: applications
Priority: optional
Maintainer: 0x53a
License: MIT
Architecture: ${ARCH}
Depends: libc6
Installed-Size: ${INSTALLED_SIZE}
EOF

cat > "$CTRL_DIR/prerm" <<'EOF'
#!/bin/sh
set -e
killall hoki-egui-demo 2>/dev/null || true
EOF
chmod 755 "$CTRL_DIR/prerm"

(cd "$CTRL_DIR" && tar czf "$PKG_DIR/control.tar.gz" .)

# --- debian-binary ---
echo "2.0" > "$PKG_DIR/debian-binary"

# --- assemble .opk ---
OUTPUT="$SRCDIR/${PKG}_${VERSION}_${ARCH}.opk"
(cd "$PKG_DIR" && ar rc "$OUTPUT" debian-binary control.tar.gz data.tar.gz)

echo "Built: $OUTPUT ($(du -h "$OUTPUT" | cut -f1))"
echo ""
echo "Install on watch:"
echo "  scp $OUTPUT root@hoki.local:/tmp/"
echo "  ssh root@hoki.local 'opkg install /tmp/$(basename "$OUTPUT")'"
