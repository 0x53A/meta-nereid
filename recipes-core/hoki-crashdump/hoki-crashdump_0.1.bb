SUMMARY = "Bounded persistent Linux and Qualcomm crash dumps for Hoki"
LICENSE = "CLOSED"
COMPATIBLE_MACHINE = "^hoki$"
SRC_URI = "file://hoki-crashdump file://hoki-crashdump.service file://99-hoki-crashdump.conf"
S = "${UNPACKDIR}"

inherit systemd
SYSTEMD_SERVICE:${PN} = "hoki-crashdump.service"
RDEPENDS:${PN} = "python3-core python3-fcntl python3-json python3-io python3-ctypes e2fsprogs-mke2fs util-linux-mount"

do_install() {
    install -Dm0755 ${S}/hoki-crashdump ${D}${sbindir}/hoki-crashdump
    install -Dm0644 ${S}/hoki-crashdump.service ${D}${systemd_system_unitdir}/hoki-crashdump.service
    install -Dm0644 ${S}/99-hoki-crashdump.conf ${D}${sysconfdir}/sysctl.d/99-hoki-crashdump.conf
}
