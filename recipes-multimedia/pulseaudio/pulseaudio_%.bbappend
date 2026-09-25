FILESEXTRAPATHS:prepend := "${THISDIR}/files:"
SRC_URI:append:hoki = " file://20-hoki-session.conf"

# Build the manually loaded RAOP sink; do not enable automatic sink discovery.
DEPENDS:append:hoki = " openssl"
EXTRA_OEMESON:remove:hoki = "-Dopenssl=disabled"
EXTRA_OEMESON:append:hoki = " -Dopenssl=enabled"

do_install:append:hoki() {
    install -d ${D}${sysconfdir}/pulse/daemon.conf.d
    install -m 0644 ${UNPACKDIR}/20-hoki-session.conf ${D}${sysconfdir}/pulse/daemon.conf.d/20-hoki-session.conf
}

FILES:${PN}-server:append:hoki = " ${sysconfdir}/pulse/daemon.conf.d/20-hoki-session.conf"
