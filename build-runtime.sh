#!/usr/bin/env bash
# Build the local UI payload using each project's own cross compilation shell.
set -euo pipefail
root=$(cd "$(dirname "$0")" && pwd)
# Cargo serializes compilation, but ELF patching/staging happens after its lock
# is released. Keep concurrent bundle builders from modifying the same binaries.
mkdir -p "$root/build"
exec 9>"$root/build/runtime-build.lock"
flock 9
python3 "$root/check-runtime.py"
source_fingerprint=$(python3 "$root/source-fingerprint.py")
export RUSTUP_TOOLCHAIN=stable
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
export CARGO_TARGET_DIR="$root/projects/target"
rustup target add armv7-unknown-linux-gnueabihf
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
# Older installed rustup toolchains refer to a garbage-collected bundled lld.
# Use the host shell's binutils linker for native build scripts; ARM keeps its
# project-specific cross linker.
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$root/host-linker.sh"
payload=$stage/hoki-runtime
mkdir -p "$payload/usr/local/bin" "$payload/usr/lib" "$payload/usr/share/hoki" "$payload/usr/lib/systemd/system" "$payload/etc/dbus-1/system.d" "$payload/usr/share/dbus-1/system-services"
while IFS='|' read -r source project dest; do
    [[ -z "$source" || "$source" == \#* ]] && continue
    printf 'Building runtime project: %s (%s)\n' "$project" "$source"
    (
        cd "$root/projects/$source"
        if [ "$project" = hoki-wasm-host ]; then
            # Rebuild the embedded guest instead of shipping an old guest.wasm.
            nix-shell --run 'cargo build --locked --release --manifest-path ../hoki-wasm-guest/Cargo.toml --target wasm32-unknown-unknown'
            if ! cmp -s "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/hoki_wasm_guest.wasm" guest.wasm; then
                install -m 0644 "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/hoki_wasm_guest.wasm" guest.wasm
            fi
        fi
        nix-shell --run 'export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="$CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS -C link-arg=-fuse-ld=bfd"; cargo build --locked --release --target armv7-unknown-linux-gnueabihf'
        HOKI_ELF_PATCHER="$root/patch-watch-elf.sh" \
        HOKI_ELF_TARGET="$CARGO_TARGET_DIR/armv7-unknown-linux-gnueabihf/release/$project" \
            nix-shell -p patchelf --run 'bash "$HOKI_ELF_PATCHER" "$HOKI_ELF_TARGET"'
    )
    install -m 0755 "$CARGO_TARGET_DIR/armv7-unknown-linux-gnueabihf/release/$project" "$payload/$dest/"
    if [ -f "$root/projects/$source/deploy/$project.desktop" ]; then
        install -Dm0644 "$root/projects/$source/deploy/$project.desktop" "$payload/usr/share/applications/$project.desktop"
        launcher="$root/projects/$source/deploy/$project"
        [ -f "$launcher" ] || launcher="$launcher.sh"
        install -Dm0755 "$launcher" "$payload/usr/bin/$project"
    fi
done < "$root/runtime-projects.txt"
install -Dm0644 "$root/projects/hoki-clock/deploy/hoki-clockd.service" "$payload/usr/lib/systemd/system/hoki-clockd.service"
install -Dm0644 "$root/projects/hoki-assistant/deploy/org.hoki.assistant.conf" "$payload/etc/dbus-1/system.d/org.hoki.assistant.conf"
install -Dm0644 "$root/projects/hoki-powerd/deploy/suspend-gate.conf" "$payload/usr/lib/systemd/system/systemd-suspend.service.d/50-hoki-powerd.conf"
install -Dm0644 "$root/projects/hoki-powerd/deploy/30-hoki-inhibitors.rules" "$payload/usr/share/polkit-1/rules.d/30-hoki-inhibitors.rules"
for project in hoki-powerd hoki-radiod; do
    install -m 0644 "$root/projects/$project/deploy/$project.service" "$payload/usr/lib/systemd/system/"
    install -m 0644 "$root/projects/$project/deploy/"org.hoki.*.conf "$payload/etc/dbus-1/system.d/"
    install -m 0644 "$root/projects/$project/deploy/"org.hoki.*.service "$payload/usr/share/dbus-1/system-services/"
done
for face in hoki-digital hoki-seconds hoki-orbit hoki-instrument; do
    install -Dm0644 "$root/projects/hoki-lp-watchface/deploy/$face.json" "$payload/usr/share/hoki/ambient-faces/$face.json"
done
install -m 0644 "$root/projects/nereid-compositor/opk/hoki-rsb-enable.service" "$payload/usr/lib/systemd/system/"
for manager in system user; do
    for state in online offline; do
        install -Dm0644 "$root/projects/hoki-networkd/deploy/hoki-network-$state.target" "$payload/usr/lib/systemd/$manager/hoki-network-$state.target"
    done
done
install -Dm0644 "$root/projects/hoki-networkd/deploy/hoki-networkd.service" "$payload/usr/lib/systemd/system/hoki-networkd.service"
install -Dm0644 "$root/projects/hoki-networkd/deploy/hoki-networkd-user.service" "$payload/usr/lib/systemd/user/hoki-networkd.service"
install -Dm0644 "$root/projects/hoki-connect/deploy/hoki-connect.service" "$payload/usr/lib/systemd/user/hoki-connect.service"
install -Dm0644 "$root/projects/hoki-music/deploy/hoki-music.service" "$payload/usr/lib/systemd/user/hoki-music.service"
install -d "$payload/usr/libexec/hoki-activity" "$payload/usr/share/hoki-activity"
install -m0644 "$root/projects/hoki-activity/daemon.py" "$root/projects/hoki-activity/activity.py" "$root/projects/hoki-health-recorder/deploy/health_client.py" "$root/projects/hoki-health-recorder/deploy/power_client.py" "$payload/usr/libexec/hoki-activity/"
install -m0644 "$root/projects/hoki-activity/README.md" "$root/projects/hoki-activity/export.py" "$payload/usr/share/hoki-activity/"
install -Dm0644 "$root/projects/hoki-activity/deploy/hoki-activity.service" "$payload/usr/lib/systemd/user/hoki-activity.service"
mkdir -p "$payload/etc/systemd/system/multi-user.target.wants" "$payload/etc/systemd/user/default.target.wants" "$payload/etc/systemd/user/hoki-network-online.target.wants"
ln -s /usr/lib/systemd/system/hoki-networkd.service "$payload/etc/systemd/system/multi-user.target.wants/hoki-networkd.service"
ln -s /usr/lib/systemd/user/hoki-networkd.service "$payload/etc/systemd/user/default.target.wants/hoki-networkd.service"
ln -s /usr/lib/systemd/user/hoki-connect.service "$payload/etc/systemd/user/hoki-network-online.target.wants/hoki-connect.service"
(cd "$payload" && find usr/local/bin usr/lib -maxdepth 1 -type f -print0 | sort -z | xargs -0 sha256sum > usr/share/hoki/runtime-sha256.txt)
[ "$source_fingerprint" = "$(python3 "$root/source-fingerprint.py")" ] || {
    echo 'Runtime sources changed during build; rebuild before publishing.' >&2
    exit 1
}
printf '%s\n' "$source_fingerprint" > "$payload/usr/share/hoki/runtime-source.sha256"
# Build into a temporary archive, then atomically publish a complete payload.
archive="$root/recipes-hoki/hoki-ui/files/hoki-runtime.tar.gz"
bash "$root/publish-runtime-archive.sh" "$stage" hoki-runtime "$archive" \
    "$root/check-runtime.py"
printf 'Runtime payload ready: %s\n' "$archive"
