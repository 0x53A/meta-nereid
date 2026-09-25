SUMMARY = "Fixed read-only NDEF text card for nfcd testing"
DESCRIPTION = "Non-payment LocalHostApp test application; requires a backend with card-emulation support."
LICENSE = "BSD-3-Clause"
LIC_FILES_CHKSUM = "file://LICENSE;md5=a6a2b815ea71b7e7395486edb907d329"

FILESEXTRAPATHS:prepend := "${THISDIR}/../../projects/hoki-nfc-test-card:"
SRC_URI = "file://src file://Makefile file://LICENSE file://README.md"
S = "${UNPACKDIR}"
DEPENDS = "glib-2.0"
RDEPENDS:${PN} = "nfcd"

inherit pkgconfig

EXTRA_OEMAKE = "BINDIR=${bindir}"

do_compile() {
    oe_runmake
}

do_install() {
    oe_runmake install DESTDIR=${D}
}
