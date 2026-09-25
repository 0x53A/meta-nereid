SUMMARY = "Opt-in Hoki raw and processed health recording tools"
LICENSE = "CLOSED"
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"
SRC_URI = "file://health-recorder-runtime.tar.gz"
S = "${UNPACKDIR}"

PACKAGES =+ "${PN}-ssc"
RDEPENDS:${PN} += "${PN}-ssc sensorfw sensorfw-hybris-binder-plugins systemd"
# SSC uses the device's Android/bionic ABI and vendor libraries, outside the
# glibc package namespace. Keep this exception scoped to that helper alone.
INSANE_SKIP:${PN}-ssc += "file-rdeps"
INSANE_SKIP:${PN} += "already-stripped"
INHIBIT_PACKAGE_STRIP = "1"
INHIBIT_PACKAGE_DEBUG_SPLIT = "1"
do_configure[noexec] = "1"
do_compile[noexec] = "1"

do_install() {
    cp -R --no-preserve=ownership ${UNPACKDIR}/health-recorder-runtime/. ${D}/
}
FILES:${PN}-ssc = "${libexecdir}/hoki-ssc-recorder"
FILES:${PN} += "${libexecdir}/hoki-recording-suspend-loop ${datadir}/hoki-health-recorder"
# No boot service: recording requires a prepared, owned session with independent
# recovery. Installing tools must not alter sensor configuration or power policy.
