#!/usr/bin/env bash
# Build independently of the custom UI, using the daemon's cross environment.
set -euo pipefail
root=$(cd "$(dirname "$0")" && pwd)
source_fingerprint=$(python3 "$root/ble-ssh-fingerprint.py")
export RUSTUP_TOOLCHAIN=stable
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$root/host-linker.sh"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cd "$root/projects/ble-ssh/watch-rs"
nix-shell --run 'export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="$CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS -C link-arg=-fuse-ld=bfd"; cargo build --locked --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'patchelf --set-interpreter /lib/ld-linux-armhf.so.3 --set-rpath /usr/lib:/lib target/armv7-unknown-linux-gnueabihf/release/ble-ssh-watch'
payload=$stage/ble-ssh-runtime
install -Dm0755 target/armv7-unknown-linux-gnueabihf/release/ble-ssh-watch "$payload/usr/bin/ble-ssh-watch"
install -Dm0644 ble-ssh-watch.service "$payload/usr/lib/systemd/system/ble-ssh-watch.service"
install -Dm0644 ble-ssh-watch.env "$payload/etc/default/ble-ssh-watch"
install -Dm0644 com.ble_ssh.conf "$payload/etc/dbus-1/system.d/com.ble_ssh.conf"
mkdir -p "$payload/usr/share/ble-ssh"
[ "$source_fingerprint" = "$(python3 "$root/ble-ssh-fingerprint.py")" ] || {
    echo 'Bluetooth SSH sources changed during build; rebuild before publishing.' >&2
    exit 1
}
printf '%s\n' "$source_fingerprint" > "$payload/usr/share/ble-ssh/source.sha256"
archive="$root/recipes-connectivity/ble-ssh/files/ble-ssh-runtime.tar.gz"
tar -C "$stage" -czf "$archive.tmp" ble-ssh-runtime
mv "$archive.tmp" "$archive"
printf 'Bluetooth SSH payload ready: %s\n' "$archive"
