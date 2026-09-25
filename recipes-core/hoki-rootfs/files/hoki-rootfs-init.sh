# Sourced by Asteroid's initramfs after mounting userdata on /sdcard.
# Return 0: managed root mounted; 1: boot legacy root; 2: stop in recovery.
hoki_valid_version() {
    case "$1" in ''|-*|*[!a-zA-Z0-9_-]*|legacy) return 1;; esac
    [ "${#1}" -le 64 ]
}

hoki_read_selection() {
    [ -f "$1/selection" ] && [ ! -L "$1/selection" ] || return 1
    IFS=' ' read -r HOKI_GOOD HOKI_TRIAL HOKI_EXTRA < "$1/selection" || return 1
    [ -z "$HOKI_EXTRA" ] || return 1
    [ "$(cat "$1/selection")" = "$HOKI_GOOD $HOKI_TRIAL" ] || return 1
    [ "$HOKI_GOOD" = legacy ] || hoki_valid_version "$HOKI_GOOD" || return 1
    [ "$HOKI_TRIAL" = - ] || hoki_valid_version "$HOKI_TRIAL" || return 1
    [ "$(wc -l < "$1/selection")" -eq 1 ] || return 1
}

hoki_mount_version() {
    hoki_valid_version "$1" || return 1
    local version="$1" store=/sdcard/.hoki dir digest bytes actual target name
    dir="$store/versions/$version"
    [ -d "$dir" ] && [ ! -L "$dir" ] || return 1
    for name in rootfs.ext4 recovery.sha256 recovery.size; do
        [ -f "$dir/$name" ] && [ ! -L "$dir/$name" ] || return 1
    done
    digest=$(cat "$dir/recovery.sha256")
    bytes=$(cat "$dir/recovery.size")
    case "$digest" in *[!0-9a-f]*|'') return 1;; esac
    [ "${#digest}" -eq 64 ] || return 1
    case "$bytes" in *[!0-9]*|'') return 1;; esac
    [ "${#bytes}" -le 8 ] && [ "$bytes" -gt 0 ] && [ "$bytes" -le 33554432 ] || return 1
    # Match the complete Android boot image, including this initramfs. No
    # uname-only ABI check: kernels with the same release can be incompatible.
    # Minimal initramfs BusyBox has dd, but head lacks FEATURE_FANCY_HEAD.
    actual=$(dd if=/dev/mmcblk0p29 bs="$bytes" count=1 2>/dev/null | sha256sum)
    [ "${actual%% *}" = "$digest" ] || return 1
    mkdir -p /hoki-lower /loop "$dir/upper" "$dir/work" || return 1
    mount -t ext4 -o ro,noload,loop "$dir/rootfs.ext4" /hoki-lower || return 1
    if ! mount -t overlay overlay -o "lowerdir=/hoki-lower,upperdir=$dir/upper,workdir=$dir/work" /loop; then
        umount /hoki-lower
        return 1
    fi
    # Once the overlay is mounted, any failure is fatal: do not stack another
    # root on top of partially mounted state. Caller uses recovery on status 2.
    while IFS=' ' read -r name target; do
        [ -d "$store/state/$name" ] && [ ! -L "$store/state/$name" ] || return 2
        [ ! -L "/loop/$target" ] || return 2
        mkdir -p "/loop/$target" || return 2
        mount --bind "$store/state/$name" "/loop/$target" || return 2
    done < /hoki-state-paths
    # The shipped fstab describes direct block-device boot. Do not let systemd
    # fsck/remount /dev/root as if it were this OverlayFS root.
    awk '$2 != "/" { print }' /loop/etc/fstab > /loop/etc/fstab.hoki || return 2
    mv /loop/etc/fstab.hoki /loop/etc/fstab || return 2
    # Persist identity files individually; sshd configuration stays versioned.
    for name in ssh_host_ecdsa_key ssh_host_ecdsa_key.pub localtime timezone machine-id; do
        case "$name" in ssh_*) target="etc/ssh/$name";; *) target="etc/$name";; esac
        [ -f "$store/state/identity/$name" ] && [ ! -L "$store/state/identity/$name" ] || return 2
        # localtime may be a symlink in the base image. Replace its overlay entry
        # before the bind so mount cannot follow it into /usr/share/zoneinfo.
        rm -f "/loop/$target" || return 2
        touch "/loop/$target" || return 2
        mount --bind "$store/state/identity/$name" "/loop/$target" || return 2
    done
    if [ -f "$store/state/tailscale/tailscaled.state" ]; then
        mkdir -p /loop/etc/systemd/system/multi-user.target.wants || return 2
        ln -sf /usr/lib/systemd/system/tailscaled.service /loop/etc/systemd/system/multi-user.target.wants/tailscaled.service || return 2
    fi
    mkdir -p /loop/userdata /loop/.hoki-lower || return 2
    printf '%s\n' "$version" > /loop/etc/hoki-rootfs-booted || return 2
    # Keep backing mounts inside the new root across switch_root.
    mount --move /hoki-lower /loop/.hoki-lower || return 2
    mount --move /sdcard /loop/userdata || return 2
    BOOT_DIR=/loop
    return 0
}

hoki_select_root() {
    local store="${1:-/sdcard/.hoki}" rc
    [ -e "$store/selection" ] || return 1
    [ -d "$store" ] && [ ! -L "$store" ] || return 2
    hoki_read_selection "$store" || return 2
    if [ "$HOKI_TRIAL" != - ]; then
        # Consume trial durably before mounting it. A later reboot goes back
        # to the confirmed version unless userspace explicitly confirms this one.
        (umask 077; printf '%s -\n' "$HOKI_GOOD" > "$store/selection.boot") || return 2
        sync
        mv -f "$store/selection.boot" "$store/selection" || return 2
        sync
        hoki_mount_version "$HOKI_TRIAL"
        rc=$?
        [ "$rc" -eq 0 ] && return 0
        [ "$rc" -eq 2 ] && return 2
    fi
    [ "$HOKI_GOOD" = legacy ] && return 1
    hoki_mount_version "$HOKI_GOOD"
    rc=$?
    [ "$rc" -eq 0 ] && return 0
    return 2
}
