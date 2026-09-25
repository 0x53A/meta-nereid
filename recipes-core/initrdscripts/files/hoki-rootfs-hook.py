"""Patch the upstream init at a checked anchor; fail on upstream drift."""
import pathlib
import sys
p = pathlib.Path(sys.argv[1])
s = p.read_text()
assert 'hoki_select_root' not in s, 'initramfs already patched'
start = 'info "Checking for loop rootfs image on the sdcard..."'
end = 'if [ ! -e $system_partition ] ; then'
assert s.count(start) == s.count(end) == 1, 'initramfs root selection anchors changed'
a, b = s.index(start), s.index(end)
legacy = s[a:b]
s = s[:a] + '''# Optional Hoki version store. Keep the upstream legacy path when absent.
. /hoki-rootfs-init.sh
hoki_select_root
hoki_result=$?
case "$hoki_result" in
0) ;;
1)
''' + legacy + ''';;
*)
    info "Managed root failed; refusing an unselected system."
    # Hoki's machine hook starts ADB when no valid machine.conf exists.
    mkdir -p /hoki-recovery
    /init.machine /hoki-recovery
    while :; do sleep 60; done
    ;;
esac

''' + s[b:]
p.write_text(s)
