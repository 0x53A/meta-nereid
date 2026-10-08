FILESEXTRAPATHS:prepend := "${THISDIR}/files:"
SRC_URI:append:hoki = " file://10-ceres-only.conf"

do_install:append:hoki() {
    install -d ${D}${systemd_user_unitdir}/timed.service.d
    install -m 0644 ${UNPACKDIR}/10-ceres-only.conf ${D}${systemd_user_unitdir}/timed.service.d/
}

FILES:${PN}:append:hoki = " ${systemd_user_unitdir}/timed.service.d"
