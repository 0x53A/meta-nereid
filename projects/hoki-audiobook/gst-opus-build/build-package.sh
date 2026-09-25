#!/bin/bash
set -euo pipefail

# Package libopus + GStreamer opus plugin as opkg for AsteroidOS

PKG="0x53a.gst-opus"
VERSION="1.24.13"
ARCH="armv7vehf-neon"
BUILDDIR="$(cd "$(dirname "$0")" && pwd)"
OUTDIR="$BUILDDIR/out"
PKG_DIR=$(mktemp -d)
trap "rm -rf $PKG_DIR" EXIT

if [ ! -f "$OUTDIR/libgstopus.so" ] || [ ! -f "$OUTDIR/libopus.so.0.10.1" ]; then
    echo "ERROR: Build output not found. Run build.sh first."
    exit 1
fi

echo "==> Packaging $PKG..."

# --- data.tar.gz ---
DATA_DIR="$PKG_DIR/data"
mkdir -p "$DATA_DIR/usr/lib"
mkdir -p "$DATA_DIR/usr/lib/gstreamer-1.0"

# libopus
cp "$OUTDIR/libopus.so.0.10.1" "$DATA_DIR/usr/lib/"
(cd "$DATA_DIR/usr/lib" && ln -sf libopus.so.0.10.1 libopus.so.0 && ln -sf libopus.so.0 libopus.so)

# GStreamer opus plugin
cp "$OUTDIR/libgstopus.so" "$DATA_DIR/usr/lib/gstreamer-1.0/"

(cd "$DATA_DIR" && tar czf "$PKG_DIR/data.tar.gz" .)

# --- control.tar.gz ---
CTRL_DIR="$PKG_DIR/control"
mkdir -p "$CTRL_DIR"

INSTALLED_SIZE=$(du -sk "$DATA_DIR" | cut -f1)

cat > "$CTRL_DIR/control" <<EOF
Package: ${PKG}
Version: ${VERSION}-r0
Description: Opus codec support for GStreamer (libopus 1.5.2 + gstopus plugin)
Section: multimedia
Priority: optional
Maintainer: 0x53a
License: BSD-3-Clause
Architecture: ${ARCH}
Depends: libc6, gstreamer1.0, gstreamer1.0-plugins-base
Installed-Size: ${INSTALLED_SIZE}
EOF

cat > "$CTRL_DIR/postinst" <<'EOF'
#!/bin/sh
set -e
# Update GStreamer plugin registry
if command -v gst-inspect-1.0 >/dev/null 2>&1; then
    gst-inspect-1.0 opusdec >/dev/null 2>&1 && echo "opusdec element registered OK" || echo "Warning: opusdec not detected, try: gst-inspect-1.0 --gst-plugin-path=/usr/lib/gstreamer-1.0"
fi
ldconfig 2>/dev/null || true
EOF
chmod 755 "$CTRL_DIR/postinst"

(cd "$CTRL_DIR" && tar czf "$PKG_DIR/control.tar.gz" .)

# --- debian-binary ---
echo "2.0" > "$PKG_DIR/debian-binary"

# --- assemble .opk ---
OUTPUT="$BUILDDIR/${PKG}_${VERSION}_${ARCH}.opk"
(cd "$PKG_DIR" && ar rc "$OUTPUT" debian-binary control.tar.gz data.tar.gz)

echo "Built: $OUTPUT ($(du -h "$OUTPUT" | cut -f1))"
echo ""
echo "Install on watch:"
echo "  scp $OUTPUT root@hoki.local:/tmp/"
echo "  ssh root@hoki.local 'opkg install /tmp/$(basename "$OUTPUT")'"
echo ""
echo "Verify:"
echo "  ssh root@hoki.local 'gst-inspect-1.0 opusdec'"
echo "  ssh root@hoki.local 'gst-launch-1.0 playbin uri=file:///home/ceres/Music/audiobooks/Piranesi.opus'"
