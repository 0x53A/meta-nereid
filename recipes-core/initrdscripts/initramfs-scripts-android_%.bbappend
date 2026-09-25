FILESEXTRAPATHS:prepend:hoki := "${THISDIR}/files:"
SRC_URI:append:hoki = " file://hoki-rootfs-hook.py"

do_install:append:hoki() {
    ${PYTHON} ${UNPACKDIR}/hoki-rootfs-hook.py ${D}/init
}
