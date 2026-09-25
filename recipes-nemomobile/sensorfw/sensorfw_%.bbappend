FILESEXTRAPATHS:prepend := "${THISDIR}:"
SRC_URI += "file://0001-qt6-dbus-optional-arguments.patch"

SRC_URI += "file://0002-validated-magnetometer-calibration.patch"

# Calibrate on demand in the shared chain. Keep background calibration disabled:
# its early sensor request failed on this HAL and needlessly samples at idle.
do_install:append:hoki() {
    sed -i 's/^needs_calibration = 0/needs_calibration = 1/' ${D}${sysconfdir}/sensorfw/primaryuse.conf
    sed -i '/^needs_calibration = 1/a calibration_file = /var/lib/sensorfw/magnetometer-calibration.json' ${D}${sysconfdir}/sensorfw/primaryuse.conf
}

# Preserve regression sources; sensorfw.inc excludes tests from image builds.
SRC_URI += " file://0005-preserve-calibration-and-recording-tests.patch"

SRC_URI += " file://0006-spo2-reading-contract.patch"
PR:append:hoki = ".spo2reading1"
