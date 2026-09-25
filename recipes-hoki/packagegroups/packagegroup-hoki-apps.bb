SUMMARY = "Local Hoki watch applications"
LICENSE = "MIT"
inherit packagegroup
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"

RDEPENDS:${PN} = "hoki-spo2 pebble-runner imu-test-app hoki-audio hoki-music hoki-audiobook \
    hoki-podcast bt-pair hoki-nfc hoki-egui-demo demo-asteroid-app hoki-wasm-host hoki-connect-ui"
