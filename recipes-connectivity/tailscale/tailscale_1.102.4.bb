SUMMARY = "Tailscale official static ARM client for Hoki"
HOMEPAGE = "https://tailscale.com"
LICENSE = "BSD-3-Clause"
LIC_FILES_CHKSUM = "file://${UNPACKDIR}/LICENSE;md5=cadeae10a8856ddfdb129866b75b33e3"
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"
SRC_URI = "https://pkgs.tailscale.com/stable/tailscale_${PV}_arm.tgz file://LICENSE file://tailscaled.service"
SRC_URI[sha256sum] = "b981a59cb85fb923ee6e1860ee6934772c83a840a6627f0dbfd7711ed690b869"
S = "${UNPACKDIR}/tailscale_${PV}_arm"
inherit systemd
SYSTEMD_SERVICE:${PN} = "tailscaled.service"
# Personalization restores this watch's private state and enables the service.
SYSTEMD_AUTO_ENABLE = "disable"
INHIBIT_PACKAGE_STRIP = "1"
INHIBIT_PACKAGE_DEBUG_SPLIT = "1"
INSANE_SKIP:${PN} += "already-stripped ldflags"

do_configure[noexec] = "1"
do_compile[noexec] = "1"
do_install() {
    install -Dm0755 ${S}/tailscale ${D}${bindir}/tailscale
    install -Dm0755 ${S}/tailscaled ${D}${sbindir}/tailscaled
    install -Dm0644 ${UNPACKDIR}/tailscaled.service ${D}${systemd_system_unitdir}/tailscaled.service
    install -d -m0700 ${D}${localstatedir}/lib/tailscale
}
