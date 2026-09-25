SUMMARY = "Versioned file-backed Hoki root filesystems"
LICENSE = "CLOSED"
COMPATIBLE_MACHINE = "^hoki$"
SRC_URI = "file://hoki-rootfs.py file://hoki-rootfs-init.sh file://state-paths file://managed-machine-id.conf"
S = "${UNPACKDIR}"
PACKAGES =+ "${PN}-initramfs"
RDEPENDS:${PN} = "python3-core python3-json python3-io python3-crypt e2fsprogs-e2fsck coreutils"
RDEPENDS:${PN}-initramfs = "busybox"
do_install() {
    install -Dm0644 ${S}/managed-machine-id.conf ${D}${systemd_system_unitdir}/systemd-machine-id-commit.service.d/managed-machine-id.conf
    install -Dm0755 ${S}/hoki-rootfs.py ${D}${sbindir}/hoki-rootfs
    install -Dm0644 ${S}/hoki-rootfs-init.sh ${D}/hoki-rootfs-init.sh
    install -Dm0644 ${S}/state-paths ${D}/hoki-state-paths
}
FILES:${PN}-initramfs = "/hoki-rootfs-init.sh /hoki-state-paths"

FILES:${PN} += "${systemd_system_unitdir}/systemd-machine-id-commit.service.d"
