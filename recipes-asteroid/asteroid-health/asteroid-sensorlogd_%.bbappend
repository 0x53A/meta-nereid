FILESEXTRAPATHS:prepend := "${THISDIR}/files:"
SRC_URI:append:hoki = " file://0001-hoki-keep-step-logger-responsive-and-null-safe.patch"
SRC_URI:append:hoki = " file://10-ceres-only.conf"

do_install:append:hoki() {
    install -Dm0644 ${UNPACKDIR}/10-ceres-only.conf \
        ${D}${systemd_user_unitdir}/asteroid-sensorlogd.service.d/10-ceres-only.conf
    # Hoki's own collection stack owns sensor recording. Mask the legacy
    # global user service, including activation through basic.target.
    install -d ${D}${sysconfdir}/systemd/user
    ln -s /dev/null ${D}${sysconfdir}/systemd/user/asteroid-sensorlogd.service
}

FILES:${PN}:append:hoki = " ${systemd_user_unitdir}/asteroid-sensorlogd.service.d"
FILES:${PN}:append:hoki = " ${sysconfdir}/systemd/user/asteroid-sensorlogd.service"
