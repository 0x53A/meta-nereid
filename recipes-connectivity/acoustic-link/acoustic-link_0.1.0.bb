SUMMARY = "Experimental multiplexed acoustic SSH link"
LICENSE = "CLOSED"
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"
SRC_URI = "file://acoustic-link-runtime.tar.gz"
S = "${UNPACKDIR}"
inherit systemd

# User services belong to the ceres PulseAudio session. Neither starts at boot.
SYSTEMD_SERVICE:${PN} = "acoustic-link.service acoustic-link-client.service"
SYSTEMD_AUTO_ENABLE = "disable"
# libquiet is loaded at runtime; pacat and parec are in pulseaudio-misc.
RDEPENDS:${PN} += "libquiet pulseaudio-server pulseaudio-misc"
INSANE_SKIP:${PN} += "already-stripped"
INHIBIT_PACKAGE_STRIP = "1"
INHIBIT_SYSROOT_STRIP = "1"

do_install() {
    cp -R --no-preserve=ownership ${UNPACKDIR}/acoustic-link-runtime/. ${D}/
}
FILES:${PN} += "${systemd_user_unitdir}/*.service ${datadir}/acoustic-link"
