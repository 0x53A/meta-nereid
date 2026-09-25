SUMMARY = "Configurable Bluetooth SSH tunnel for Hoki"
LICENSE = "CLOSED"
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"
SRC_URI = "file://ble-ssh-runtime.tar.gz"
S = "${UNPACKDIR}"
inherit systemd

SYSTEMD_SERVICE:${PN} = "ble-ssh-watch.service"
SYSTEMD_AUTO_ENABLE = "disable"
DEPENDS += "dbus"
RDEPENDS:${PN} += "bluez5 dbus dbus-lib"
# The image's existing SSH server supplies authentication and localhost:22.
CONFFILES:${PN} += "${sysconfdir}/default/ble-ssh-watch"
INSANE_SKIP:${PN} += "already-stripped"
INHIBIT_PACKAGE_STRIP = "1"
INHIBIT_SYSROOT_STRIP = "1"

do_install() {
    cp -R --no-preserve=ownership ${UNPACKDIR}/ble-ssh-runtime/. ${D}/
}
FILES:${PN} += "${systemd_system_unitdir}/ble-ssh-watch.service ${datadir}/ble-ssh"
