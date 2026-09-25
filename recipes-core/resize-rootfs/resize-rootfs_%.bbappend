# Managed roots are fixed-size read-only images under OverlayFS. Growing the
# backing userdata filesystem, if wanted, is a separate provisioning operation.
pkg_postinst_ontarget:${PN}:prepend() {
    if [ -f /etc/hoki-rootfs-booted ] && \
       [ "$(awk '$2 == "/" { print $3 }' /proc/mounts)" = overlay ]; then
        echo "Managed root image: no root filesystem resize required."
        exit 0
    fi
}
