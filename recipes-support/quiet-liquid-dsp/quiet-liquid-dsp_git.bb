SUMMARY = "quiet-liquid-dsp: pinned Quiet acoustic modem dependency"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://LICENSE;md5=860e4083ceb93ce0939b1a58fcaacb53"
SRC_URI = "git://github.com/quiet/liquid-dsp.git;protocol=https;nobranch=1"
SRCREV = "4951bbbf67a9857dbaab0bc6fa69801717308109"
PV = "0.0+git"

inherit autotools-brokensep
EXTRA_OECONF = "--enable-simdoverride --enable-fftoverride"
CFLAGS:append = " -fcommon"
# The old makefile prepends exec_prefix to an already absolute libdir.
do_configure:append() {
    sed -i 's|$(exec_prefix)$(libdir)|$(libdir)|g' ${S}/makefile
}

# Upstream ships a real, unversioned shared library, not a development symlink.
SOLIBS = ".so"
FILES_SOLIBSDEV = ""
