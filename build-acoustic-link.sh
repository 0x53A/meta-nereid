#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
source_fingerprint=$(python3 "$root/meta-nereid/acoustic-link-fingerprint.py")
export RUSTUP_TOOLCHAIN=stable
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$root/meta-nereid/host-linker.sh"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cd "$root/acoustic-link"
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../meta-nereid/patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/acoustic-link'
payload=$stage/acoustic-link-runtime
install -Dm0755 target/armv7-unknown-linux-gnueabihf/release/acoustic-link "$payload/usr/bin/acoustic-link"
for service in acoustic-link acoustic-link-client; do
    install -Dm0644 "deploy/$service.service" "$payload/usr/lib/systemd/user/$service.service"
    sed -i 's|/usr/local/bin/acoustic-link|/usr/bin/acoustic-link|' "$payload/usr/lib/systemd/user/$service.service"
done
mkdir -p "$payload/usr/share/acoustic-link"
[ "$source_fingerprint" = "$(python3 "$root/meta-nereid/acoustic-link-fingerprint.py")" ] || {
    echo 'Acoustic sources changed during build; rebuild before publishing.' >&2
    exit 1
}
printf '%s\n' "$source_fingerprint" > "$payload/usr/share/acoustic-link/source.sha256"
archive="$root/meta-nereid/recipes-connectivity/acoustic-link/files/acoustic-link-runtime.tar.gz"
tar -C "$stage" -czf "$archive.tmp" acoustic-link-runtime
mv "$archive.tmp" "$archive"
printf 'Acoustic payload ready: %s\n' "$archive"
