#!/bin/sh
# Deploy ble-ssh-watch to watch via SSH
# Usage: nix-shell --run ./deploy.sh
#
# Set WATCH_HOST to override the default hostname.
#   WATCH_HOST=192.168.1.42 nix-shell --run ./deploy.sh

set -e
cd "$(dirname "$0")"

TARGET=armv7-unknown-linux-gnueabihf
BIN=ble-ssh-watch
HOST="${WATCH_HOST:-hoki.local}"

echo "Building for $TARGET..."
cargo build --locked --release --target "$TARGET"

echo "Patching interpreter..."
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "target/$TARGET/release/$BIN"

echo "Deploying to $HOST via SSH..."
scp "target/$TARGET/release/$BIN" "root@${HOST}:/tmp/$BIN"
scp ble-ssh-watch.service "root@${HOST}:/etc/systemd/system/ble-ssh-watch.service"
scp ble-ssh-watch.env "root@${HOST}:/tmp/ble-ssh-watch.env"
scp com.ble_ssh.conf "root@${HOST}:/etc/dbus-1/system.d/com.ble_ssh.conf"

ssh "root@${HOST}" "
  set -e
  systemctl daemon-reload
  systemctl stop ble-ssh-watch
  mkdir -p /etc/default
  if [ ! -e /etc/default/ble-ssh-watch ]; then
    cp /tmp/ble-ssh-watch.env /etc/default/ble-ssh-watch
  fi
  cp /tmp/$BIN /usr/bin/$BIN
  chmod +x /usr/bin/$BIN
  rm /tmp/$BIN
  systemctl daemon-reload
  systemctl enable ble-ssh-watch
  systemctl restart ble-ssh-watch
"

echo "Done! Service started. Check with:"
echo "  ssh root@${HOST} journalctl -u ble-ssh-watch -f"
