SUMMARY = "Authenticated SSH companion status and provisioning helper"
LICENSE = "CLOSED"
FILESEXTRAPATHS:prepend := "${THISDIR}/../../projects/hoki-companion-agent:"
SRC_URI = "file://agent.py"
S = "${UNPACKDIR}"
RDEPENDS:${PN} = "python3-core python3-json python3-io python3-crypt systemd connman tzdata"
do_install() {
    install -Dm0755 ${S}/agent.py ${D}${libexecdir}/hoki-companion-agent
}
FILES:${PN} = "${libexecdir}/hoki-companion-agent"
