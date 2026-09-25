SUMMARY = "libquiet: pinned Quiet acoustic modem dependency"
LICENSE = "BSD-3-Clause"
LIC_FILES_CHKSUM = "file://LICENSE;md5=0da3ab9bb540414e574cbb2f96abcd81"
SRC_URI = "git://github.com/quiet/quiet.git;protocol=https;nobranch=1"
SRCREV = "b64a058ed40a49a8ff777bfb526f2989480eb1ec"
PV = "0.0+git"

inherit cmake
DEPENDS = "quiet-liquid-dsp jansson"
EXTRA_OECMAKE = "-DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_SKIP_RPATH=ON"
FILES:${PN} += "${datadir}/quiet"

# Upstream ships a real, unversioned shared library, not a development symlink.
SOLIBS = ".so"
FILES_SOLIBSDEV = ""
