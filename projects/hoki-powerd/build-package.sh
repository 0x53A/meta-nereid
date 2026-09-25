#!/bin/sh
set -e

PKG="0x53a.powerd"
VERSION="1.0.0"
ARCH="armv7vehf-neon"
SRCDIR="$(cd "$(dirname "$0")" && pwd)"
BINARY="$SRCDIR/target/armv7-unknown-linux-gnueabihf/release/hoki-powerd"
WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

if [ ! -f "$BINARY" ]; then
    echo "Binary not found. Build first:"
    echo "  nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'"
    exit 1
fi

# -- data.tar.gz --
mkdir -p "$WORKDIR/data/usr/local/bin"
mkdir -p "$WORKDIR/data/etc/systemd/system/multi-user.target.wants"
mkdir -p "$WORKDIR/data/etc/dbus-1/system.d"
mkdir -p "$WORKDIR/data/usr/share/dbus-1/system-services"

cp "$BINARY" "$WORKDIR/data/usr/local/bin/hoki-powerd"
chmod 755 "$WORKDIR/data/usr/local/bin/hoki-powerd"

cp "$SRCDIR/deploy/hoki-powerd.service" "$WORKDIR/data/etc/systemd/system/"
ln -s ../hoki-powerd.service "$WORKDIR/data/etc/systemd/system/multi-user.target.wants/hoki-powerd.service"

cp "$SRCDIR/deploy/org.hoki.power.conf" "$WORKDIR/data/etc/dbus-1/system.d/"
cp "$SRCDIR/deploy/org.hoki.power.service" "$WORKDIR/data/usr/share/dbus-1/system-services/"

# -- control.tar.gz --
mkdir -p "$WORKDIR/control"

cat > "$WORKDIR/control/control" <<EOF
Package: ${PKG}
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: 0x53a
Description: Power management daemon with D-Bus lease-based CPU core control
Replaces: underclock, 0x53a.underclock
Conflicts: underclock, 0x53a.underclock
Provides: underclock
EOF

cat > "$WORKDIR/control/postinst" <<'EOF'
#!/bin/sh
systemctl daemon-reload
# Stop and disable old underclock if present
systemctl stop underclock.service 2>/dev/null || true
systemctl disable underclock.service 2>/dev/null || true
rm -f /etc/udev/rules.d/99-power-cpu.rules
udevadm control --reload-rules 2>/dev/null || true
# Start the new daemon
systemctl enable --now hoki-powerd.service
EOF
chmod 755 "$WORKDIR/control/postinst"

cat > "$WORKDIR/control/prerm" <<'EOF'
#!/bin/sh
systemctl stop hoki-powerd.service 2>/dev/null || true
systemctl disable hoki-powerd.service 2>/dev/null || true
systemctl daemon-reload
EOF
chmod 755 "$WORKDIR/control/prerm"

# -- build ipk --
echo "2.0" > "$WORKDIR/debian-binary"

(cd "$WORKDIR/control" && tar czf "$WORKDIR/control.tar.gz" .)
(cd "$WORKDIR/data" && tar czf "$WORKDIR/data.tar.gz" .)

OUTPUT="$SRCDIR/${PKG}_${VERSION}_${ARCH}.ipk"
(cd "$WORKDIR" && ar r "$OUTPUT" debian-binary control.tar.gz data.tar.gz 2>/dev/null)

echo "Built: $OUTPUT"
echo "Install: scp $OUTPUT root@hoki.local:/tmp/ && ssh root@hoki.local 'opkg install /tmp/$(basename $OUTPUT)'"
