SUMMARY = "Experimental multiplexed acoustic SSH link"
HOMEPAGE = "https://github.com/0x53A/acoustic-ssh"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://LICENSE;md5=bbdc971fc4084960fa154ab447683e79"
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"
PR = "r1"
SRC_URI = "git://github.com/0x53A/acoustic-ssh.git;protocol=https;branch=main"
SRCREV = "3e8802b785e1ae65035700a7bc7d85d57e310857"
inherit cargo systemd

# The native crate has no Cargo dependencies; Cargo.lock is fetched with source.
# User services belong to the ceres PulseAudio session. Neither starts at boot.
SYSTEMD_SERVICE:${PN} = "acoustic-link.service acoustic-link-client.service"
SYSTEMD_AUTO_ENABLE = "disable"
# libquiet is loaded at runtime; pacat and parec are in pulseaudio-misc.
RDEPENDS:${PN} += "libquiet pulseaudio-server pulseaudio-misc"

do_install() {
    install -Dm0755 ${B}/target/${CARGO_TARGET_SUBDIR}/acoustic-link ${D}${bindir}/acoustic-link
    install -d ${D}${systemd_user_unitdir}
    for service in acoustic-link acoustic-link-client; do
        sed 's|/usr/local/bin/acoustic-link|${bindir}/acoustic-link|' \
            ${S}/deploy/$service.service > ${D}${systemd_user_unitdir}/$service.service
        chmod 0644 ${D}${systemd_user_unitdir}/$service.service
    done
}
FILES:${PN} += "${systemd_user_unitdir}/*.service"
