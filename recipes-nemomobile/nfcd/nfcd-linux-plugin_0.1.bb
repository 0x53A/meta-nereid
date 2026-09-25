SUMMARY = "nfcd reader backend for the Linux kernel NFC interface"
DESCRIPTION = "Uses NFC generic netlink and AF_NFC sockets; leaves controller management in the kernel."
LICENSE = "BSD-3-Clause"
LIC_FILES_CHKSUM = "file://LICENSE;md5=e93540e5cf95e06111e64368cd9b0aea"

FILESEXTRAPATHS:prepend := "${THISDIR}/../../projects/nfcd-linux-plugin:"
SRC_URI = "file://src file://Makefile file://LICENSE file://README.md"
S = "${UNPACKDIR}"
DEPENDS = "nfcd glib-2.0 libglibutil libnl"
RDEPENDS:${PN} = "nfcd"
RCONFLICTS:${PN} = "nfcd-binder-plugin nfcd-pn5xx-plugin"

inherit pkgconfig

EXTRA_OEMAKE = "PLUGIN_DIR=${libdir}/nfcd/plugins"

do_compile() {
    oe_runmake
}

do_install() {
    oe_runmake install DESTDIR=${D}
    install -d ${D}${systemd_system_unitdir}/nfcd.service.d
    cat > ${D}${systemd_system_unitdir}/nfcd.service.d/linux-backend.conf <<'EOF'
[Unit]
Conflicts=neard.service
After=neard.service
EOF
}

FILES:${PN} += "${libdir}/nfcd/plugins/linux.so ${systemd_system_unitdir}/nfcd.service.d/linux-backend.conf"
