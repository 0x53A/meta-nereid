#!/bin/sh
set -e

TARGET="armv7-unknown-linux-gnueabihf"
BINARY="target/${TARGET}/release/hoki-powerd"
HOST="root@hoki.local"

echo "==> Building..."
cargo build --release --target "$TARGET"

echo "==> Patching ELF..."
nix-shell -p patchelf --run "patchelf --set-interpreter /lib/ld-linux-armhf.so.3 --set-rpath /usr/lib:/lib $BINARY"

echo "==> Deploying to watch..."
scp "$BINARY" "${HOST}:/usr/local/bin/hoki-powerd"
scp deploy/org.hoki.power.conf "${HOST}:/etc/dbus-1/system.d/"
scp deploy/org.hoki.power.service "${HOST}:/usr/share/dbus-1/system-services/"
scp deploy/hoki-powerd.service "${HOST}:/etc/systemd/system/"

echo "==> Reloading services..."
ssh "$HOST" 'systemctl daemon-reload && systemctl enable --now hoki-powerd.service'

echo "==> Disabling old underclock..."
ssh "$HOST" 'systemctl disable underclock.service 2>/dev/null; rm -f /etc/udev/rules.d/99-power-cpu.rules; udevadm control --reload-rules' || true

echo "==> Done. Status:"
ssh "$HOST" 'systemctl status hoki-powerd.service --no-pager -l'
