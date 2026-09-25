#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
source_fingerprint=$(python3 "$root/meta-nereid/health-recorder-fingerprint.py")
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cd "$root/hoki-health-recorder"
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../meta-nereid/patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-health-recorder'
bash ssc/build.sh
payload=$stage/health-recorder-runtime
install -Dm0755 target/armv7-unknown-linux-gnueabihf/release/hoki-health-recorder "$payload/usr/bin/hoki-health-recorder"
install -Dm0755 ssc/build/hoki-ssc-recorder "$payload/usr/libexec/hoki-ssc-recorder"
install -Dm0755 deploy/suspend-loop.sh "$payload/usr/libexec/hoki-recording-suspend-loop"
install -Dm0644 README.md "$payload/usr/share/hoki-health-recorder/README.md"
install -Dm0644 CAPABILITIES.md "$payload/usr/share/hoki-health-recorder/CAPABILITIES.md"
[ "$source_fingerprint" = "$(python3 "$root/meta-nereid/health-recorder-fingerprint.py")" ] || {
    echo 'Health recorder sources changed during build; rebuild before publishing.' >&2
    exit 1
}
printf '%s\n' "$source_fingerprint" > "$payload/usr/share/hoki-health-recorder/source.sha256"
(
    cd "$payload"
    sha256sum usr/bin/hoki-health-recorder usr/libexec/hoki-ssc-recorder usr/libexec/hoki-recording-suspend-loop > usr/share/hoki-health-recorder/binaries.sha256
)
archive="$root/meta-nereid/recipes-hoki/hoki-health-recorder/files/health-recorder-runtime.tar.gz"
bash "$root/meta-nereid/publish-runtime-archive.sh" "$stage" health-recorder-runtime "$archive" \
    "$root/meta-nereid/check-health-recorder.py"
printf 'Health recorder payload ready: %s\n' "$archive"
