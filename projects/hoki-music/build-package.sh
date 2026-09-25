#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
binary=target/armv7-unknown-linux-gnueabihf/release/hoki-music
[[ -f "$binary" ]] || { echo 'Build the ARM release first.' >&2; exit 1; }
tls_deps=""
if patchelf --print-needed "$binary" | grep -q '^libssl.so.3$'; then
    tls_deps=", libssl3, libcrypto3, ca-certificates"
fi
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
install -Dm755 "$binary" "$stage/data/usr/lib/hoki-music"
bash ../../patch-watch-elf.sh "$stage/data/usr/lib/hoki-music"
install -Dm755 deploy/hoki-music.sh "$stage/data/usr/bin/hoki-music"
install -Dm644 deploy/hoki-music.desktop "$stage/data/usr/share/applications/hoki-music.desktop"
install -Dm644 deploy/hoki-music.service "$stage/data/usr/lib/systemd/user/hoki-music.service"
mkdir -p "$stage/control"
cat > "$stage/control/control" <<CONTROL
Package: 0x53a.music
Version: 0.1.1-r0
Architecture: armv7vehf-neon
Description: Rust music player with local files and Navidrome
Maintainer: 0x53a
Section: multimedia
Priority: optional
Depends: libc6, libpulse0, pulseaudio-server, libfontconfig1, libxkbcommon0, wayland${tls_deps}
CONTROL
cat > "$stage/control/postinst" <<'POSTINST'
#!/bin/sh
systemctl --user -M ceres@ daemon-reload || true
POSTINST
cat > "$stage/control/prerm" <<'PRERM'
#!/bin/sh
systemctl --user -M ceres@ stop hoki-music.service || true
PRERM
chmod 755 "$stage/control/postinst" "$stage/control/prerm"
tar -C "$stage/data" -czf "$stage/data.tar.gz" .
tar -C "$stage/control" -czf "$stage/control.tar.gz" .
printf '2.0\n' > "$stage/debian-binary"
output="$PWD/0x53a.music_0.1.1_armv7vehf-neon.opk"
(cd "$stage" && ar r "$output" debian-binary control.tar.gz data.tar.gz)
printf '%s\n' "$output"
