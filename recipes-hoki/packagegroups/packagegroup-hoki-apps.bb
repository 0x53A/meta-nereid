SUMMARY = "Local Hoki watch applications"
LICENSE = "MIT"
inherit packagegroup
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"

RDEPENDS:${PN} = "hoki-clock hoki-activity hoki-spo2 pebble-runner imu-test-app hoki-audio hoki-music hoki-audiobook \
    hoki-podcast bt-pair hoki-nfc hoki-egui-demo demo-asteroid-app hoki-wasm-host hoki-connect-ui hoki-argyroneta hoki-home-assistant asteroid-health"
RDEPENDS:${PN}:remove = "${@'hoki-wasm-host' if d.getVar('HOKI_WASM_DEMO') != '1' else ''}"
