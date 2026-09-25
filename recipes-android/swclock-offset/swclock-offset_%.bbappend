FILESEXTRAPATHS:prepend := "${THISDIR}/files:"
SRC_URI += "file://saved-offset.conf"
do_install:append() {
    install -Dm0644 ${UNPACKDIR}/saved-offset.conf ${D}${systemd_system_unitdir}/swclock-offset-boot.service.d/saved-offset.conf
}
FILES:${PN} += "${systemd_system_unitdir}/swclock-offset-boot.service.d"
