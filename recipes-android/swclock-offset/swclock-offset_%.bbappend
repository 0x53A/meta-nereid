FILESEXTRAPATHS:prepend := "${THISDIR}/files:"
SRC_URI += "file://saved-offset.conf file://swclock-offset-sync.path file://swclock-offset-sync.service"
SYSTEMD_SERVICE:${PN} += "swclock-offset-sync.path"
do_install:append() {
    install -Dm0644 ${UNPACKDIR}/saved-offset.conf ${D}${systemd_system_unitdir}/swclock-offset-boot.service.d/saved-offset.conf
    install -Dm0644 ${UNPACKDIR}/swclock-offset-sync.path ${D}${systemd_system_unitdir}/swclock-offset-sync.path
    install -Dm0644 ${UNPACKDIR}/swclock-offset-sync.service ${D}${systemd_system_unitdir}/swclock-offset-sync.service
}
FILES:${PN} += "${systemd_system_unitdir}/swclock-offset-boot.service.d"
FILES:${PN} += "${systemd_system_unitdir}/swclock-offset-sync.path ${systemd_system_unitdir}/swclock-offset-sync.service"
