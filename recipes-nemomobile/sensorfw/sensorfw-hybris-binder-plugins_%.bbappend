FILESEXTRAPATHS:prepend := "${THISDIR}:"
SRC_URI += "file://0003-manual-calibration-uncalibrated-input.patch"

# Preserve the tested opt-in raw recorder across image rebuilds.
SRC_URI:append:hoki = " file://0004-shared-raw-recording.patch"

# Failed storage now explicitly terminates capture and releases its wake hold.
PR:append:hoki = ".recording3.spo2reading1"

# Preserve regression sources; sensorfw.inc excludes tests from image builds.
SRC_URI += " file://0005-preserve-calibration-and-recording-tests.patch"

SRC_URI += " file://0006-spo2-reading-contract.patch"
