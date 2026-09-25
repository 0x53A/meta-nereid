#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")" && pwd)
source_fingerprint=$(python3 "$root/health-recorder-fingerprint.py")
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cd "$root/projects/hoki-health-recorder"
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-health-recorder'
bash ssc/build.sh
payload=$stage/health-recorder-runtime
install -Dm0755 target/armv7-unknown-linux-gnueabihf/release/hoki-health-recorder "$payload/usr/bin/hoki-health-recorder"
install -Dm0755 ssc/build/hoki-ssc-recorder "$payload/usr/libexec/hoki-ssc-recorder"
install -Dm0755 deploy/suspend-loop.sh "$payload/usr/libexec/hoki-recording-suspend-loop"
install -Dm0755 deploy/recording-session.py "$payload/usr/libexec/hoki-recording-session"
install -Dm0644 deploy/hoki-health-recording.service "$payload/usr/lib/systemd/system/hoki-health-recording.service"
install -Dm0644 deploy/30-hoki-health-recording.rules "$payload/usr/share/polkit-1/rules.d/30-hoki-health-recording.rules"
install -Dm0644 README.md "$payload/usr/share/hoki-health-recorder/README.md"
install -Dm0644 CAPABILITIES.md "$payload/usr/share/hoki-health-recorder/CAPABILITIES.md"
[ "$source_fingerprint" = "$(python3 "$root/health-recorder-fingerprint.py")" ] || {
    echo 'Health recorder sources changed during build; rebuild before publishing.' >&2
    exit 1
}
printf '%s\n' "$source_fingerprint" > "$payload/usr/share/hoki-health-recorder/source.sha256"
(
    cd "$payload"
    sha256sum usr/bin/hoki-health-recorder usr/libexec/hoki-ssc-recorder usr/libexec/hoki-recording-suspend-loop usr/libexec/hoki-recording-session > usr/share/hoki-health-recorder/binaries.sha256
)
archive="$root/recipes-hoki/hoki-health-recorder/files/health-recorder-runtime.tar.gz"
bash "$root/publish-runtime-archive.sh" "$stage" health-recorder-runtime "$archive" \
    "$root/check-health-recorder.py"
printf 'Health recorder payload ready: %s\n' "$archive"
