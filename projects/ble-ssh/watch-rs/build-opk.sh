#!/bin/sh
set -e

PKG="0x53a.ble-ssh-watch"
VERSION="0.2.0"
ARCH="armv7vehf-neon"
TARGET="armv7-unknown-linux-gnueabihf"
SRCDIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SRCDIR"
WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

# -- build the binary --
echo "Building for $TARGET..."
cargo build --locked --release --target "$TARGET"

echo "Patching interpreter..."
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "target/$TARGET/release/ble-ssh-watch"

# -- data.tar.gz: files to install --
mkdir -p "$WORKDIR/data/usr/bin" "$WORKDIR/data/etc/default"
cp "$SRCDIR/ble-ssh-watch.env" "$WORKDIR/data/etc/default/ble-ssh-watch"
mkdir -p "$WORKDIR/data/etc/systemd/system/multi-user.target.wants"
mkdir -p "$WORKDIR/data/etc/dbus-1/system.d"

cp "target/$TARGET/release/ble-ssh-watch" "$WORKDIR/data/usr/bin/ble-ssh-watch"
chmod 755 "$WORKDIR/data/usr/bin/ble-ssh-watch"

cp "$SRCDIR/ble-ssh-watch.service" "$WORKDIR/data/etc/systemd/system/ble-ssh-watch.service"
ln -s ../ble-ssh-watch.service "$WORKDIR/data/etc/systemd/system/multi-user.target.wants/ble-ssh-watch.service"

cp "$SRCDIR/com.ble_ssh.conf" "$WORKDIR/data/etc/dbus-1/system.d/com.ble_ssh.conf"

# -- control.tar.gz: package metadata --
mkdir -p "$WORKDIR/control"

echo /etc/default/ble-ssh-watch > "$WORKDIR/control/conffiles"

cat > "$WORKDIR/control/control" <<EOF
Package: ${PKG}
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: 0x53a
Description: SSH over Bluetooth (BLE GATT + L2CAP classic) tunnel daemon
EOF

cat > "$WORKDIR/control/postinst" <<'EOF'
#!/bin/sh
systemctl daemon-reload
systemctl restart ble-ssh-watch 2>/dev/null || true
EOF
chmod 755 "$WORKDIR/control/postinst"

cat > "$WORKDIR/control/prerm" <<'EOF'
#!/bin/sh
systemctl stop ble-ssh-watch 2>/dev/null || true
systemctl daemon-reload
EOF
chmod 755 "$WORKDIR/control/prerm"

# -- build the ipk --
echo "2.0" > "$WORKDIR/debian-binary"

(cd "$WORKDIR/control" && tar czf "$WORKDIR/control.tar.gz" .)
(cd "$WORKDIR/data" && tar czf "$WORKDIR/data.tar.gz" .)

OUTDIR="$SRCDIR/../../../build/packages"
mkdir -p "$OUTDIR"
OUTPUT="$OUTDIR/${PKG}_${VERSION}_${ARCH}.ipk"
(cd "$WORKDIR" && ar r "$OUTPUT" debian-binary control.tar.gz data.tar.gz 2>/dev/null)

echo "Built: $OUTPUT"
