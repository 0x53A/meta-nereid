SUMMARY = "Kitty and Ghostty terminal descriptions"
LICENSE = "MIT & GPL-3.0-only"
LIC_FILES_CHKSUM = "file://LICENSE.ghostty;md5=fec72c65cb617162437d8cff54b7c721 file://LICENSE.kitty;md5=1ebbd3e34237af26da5dc08a4e440464"
SRC_URI = "file://ghostty.terminfo file://kitty.terminfo file://LICENSE.ghostty file://LICENSE.kitty"
S = "${UNPACKDIR}"
inherit allarch
DEPENDS = "ncurses-native"

do_compile() {
    install -d ${B}/terminfo
    tic -x -o ${B}/terminfo ${S}/ghostty.terminfo
    tic -x -o ${B}/terminfo ${S}/kitty.terminfo
}
do_install() {
    install -d ${D}${datadir}/terminfo
    cp -R ${B}/terminfo/. ${D}${datadir}/terminfo/
}
FILES:${PN} = "${datadir}/terminfo"
