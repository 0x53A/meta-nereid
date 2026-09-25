#!/usr/bin/env bash
# Build the local UI payload using each project's own cross compilation shell.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
python3 "$root/meta-nereid/check-runtime.py"
source_fingerprint=$(python3 "$root/meta-nereid/source-fingerprint.py")
export RUSTUP_TOOLCHAIN=stable
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
rustup target add armv7-unknown-linux-gnueabihf
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
# Older installed rustup toolchains refer to a garbage-collected bundled lld.
# Use the host shell's binutils linker for native build scripts; ARM keeps its
# project-specific cross linker.
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$root/meta-nereid/host-linker.sh"
payload=$stage/hoki-runtime
mkdir -p "$payload/usr/local/bin" "$payload/usr/lib" "$payload/usr/share/hoki" "$payload/usr/lib/systemd/system" "$payload/etc/dbus-1/system.d" "$payload/usr/share/dbus-1/system-services"
while IFS='|' read -r source project dest; do
    [[ -z "$source" || "$source" == \#* ]] && continue
    printf 'Building runtime project: %s (%s)\n' "$project" "$source"
    (
        cd "$root/$source"
        if [ "$project" = hoki-wasm-host ]; then
            # Rebuild the embedded guest instead of shipping an old guest.wasm.
            nix-shell --run 'cargo build --locked --release --manifest-path ../hoki-wasm-guest/Cargo.toml --target wasm32-unknown-unknown'
            if ! cmp -s ../hoki-wasm-guest/target/wasm32-unknown-unknown/release/hoki_wasm_guest.wasm guest.wasm; then
                install -m 0644 ../hoki-wasm-guest/target/wasm32-unknown-unknown/release/hoki_wasm_guest.wasm guest.wasm
            fi
        fi
        nix-shell --run 'export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="$CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS -C link-arg=-fuse-ld=bfd"; cargo build --locked --release --target armv7-unknown-linux-gnueabihf'
        HOKI_ELF_PATCHER="$root/meta-nereid/patch-watch-elf.sh" \
        HOKI_ELF_TARGET="target/armv7-unknown-linux-gnueabihf/release/$project" \
            nix-shell -p patchelf --run 'bash "$HOKI_ELF_PATCHER" "$HOKI_ELF_TARGET"'
    )
    install -m 0755 "$root/$source/target/armv7-unknown-linux-gnueabihf/release/$project" "$payload/$dest/"
    if [ -f "$root/$source/deploy/$project.desktop" ]; then
        install -Dm0644 "$root/$source/deploy/$project.desktop" "$payload/usr/share/applications/$project.desktop"
        launcher="$root/$source/deploy/$project"
        [ -f "$launcher" ] || launcher="$launcher.sh"
        install -Dm0755 "$launcher" "$payload/usr/bin/$project"
    fi
done < "$root/meta-nereid/runtime-projects.txt"
for project in hoki-powerd hoki-radiod; do
    install -m 0644 "$root/$project/deploy/$project.service" "$payload/usr/lib/systemd/system/"
    install -m 0644 "$root/$project/deploy/"org.hoki.*.conf "$payload/etc/dbus-1/system.d/"
    install -m 0644 "$root/$project/deploy/"org.hoki.*.service "$payload/usr/share/dbus-1/system-services/"
done
install -m 0644 "$root/asteroid-compositor/opk/hoki-rsb-enable.service" "$payload/usr/lib/systemd/system/"
install -Dm0644 "$root/hoki-connect/deploy/hoki-connect.service" "$payload/usr/lib/systemd/user/hoki-connect.service"
install -Dm0644 "$root/hoki-music/deploy/hoki-music.service" "$payload/usr/lib/systemd/user/hoki-music.service"
(cd "$payload" && find usr/local/bin usr/lib -maxdepth 1 -type f -print0 | sort -z | xargs -0 sha256sum > usr/share/hoki/runtime-sha256.txt)
[ "$source_fingerprint" = "$(python3 "$root/meta-nereid/source-fingerprint.py")" ] || {
    echo 'Runtime sources changed during build; rebuild before publishing.' >&2
    exit 1
}
printf '%s\n' "$source_fingerprint" > "$payload/usr/share/hoki/runtime-source.sha256"
# Build into a temporary archive, then atomically publish a complete payload.
archive="$root/meta-nereid/recipes-hoki/hoki-ui/files/hoki-runtime.tar.gz"
bash "$root/meta-nereid/publish-runtime-archive.sh" "$stage" hoki-runtime "$archive" \
    "$root/meta-nereid/check-runtime.py"
printf 'Runtime payload ready: %s\n' "$archive"
