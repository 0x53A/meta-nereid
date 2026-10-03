SUMMARY = "Opt-in Hoki raw and processed health recording tools"
LICENSE = "CLOSED"
PR = "r1"
COMPATIBLE_MACHINE = "^hoki$"
inherit cargo pkgconfig systemd nereid-cargo-sbom
SYSTEMD_SERVICE:${PN} = "hoki-health-policy.service"
SYSTEMD_AUTO_ENABLE = "enable"
PACKAGE_ARCH = "${MACHINE_ARCH}"
FILESEXTRAPATHS:prepend := "${THISDIR}/../../projects:"
require health-sources.inc
S = "${UNPACKDIR}/projects/hoki-health-recorder"
DEPENDS += "android-ndk-native"
NDK_TOOLCHAIN = "${STAGING_LIBDIR_NATIVE}/android-ndk/toolchains/llvm/prebuilt/linux-x86_64"


PACKAGES =+ "${PN}-ssc"
# subprocess, pathlib and signal are supplied by python3-core in Whinlatter.
RDEPENDS:${PN} += "${PN}-ssc hoki-ui sensorfw sensorfw-hybris-binder-plugins systemd python3-core python3-json python3-fcntl python3-threading python3-netserver python3-io python3-netclient python3-math python3-datetime polkit"
# SSC uses the device's Android/bionic ABI and vendor libraries, outside the
# glibc package namespace. Keep this exception scoped to that helper alone.
INSANE_SKIP:${PN}-ssc += "file-rdeps"
INSANE_SKIP:${PN} += "already-stripped"
INHIBIT_PACKAGE_STRIP = "1"
INHIBIT_PACKAGE_DEBUG_SPLIT = "1"
do_compile:append() {
    ${NDK_TOOLCHAIN}/bin/clang --target=armv7a-linux-androideabi28 \
        -std=c11 -Wall -Wextra -Werror -pthread -O2 \
        -ffile-prefix-map=${WORKDIR}=/usr/src/debug/${PN}/${PV} \
        ${S}/ssc/collector.c -ldl -o ${B}/hoki-ssc-recorder
}

do_install() {
    install -Dm0755 ${B}/target/${CARGO_TARGET_SUBDIR}/hoki-health-recorder ${D}${bindir}/hoki-health-recorder
    install -Dm0755 ${B}/hoki-ssc-recorder ${D}${libexecdir}/hoki-ssc-recorder
    install -Dm0755 ${S}/deploy/suspend-loop.sh ${D}${libexecdir}/hoki-recording-suspend-loop
    install -Dm0755 ${S}/deploy/recording-session.py ${D}${libexecdir}/hoki-recording-session
    install -Dm0755 ${S}/deploy/health-policy.py ${D}${libexecdir}/hoki-health-policy
    install -Dm0755 ${S}/deploy/manual-consumer.py ${D}${libexecdir}/hoki-manual-consumer
    install -Dm0644 ${S}/deploy/health_broker.py ${D}${libexecdir}/health_broker.py
    install -Dm0644 ${S}/deploy/health_client.py ${D}${libexecdir}/health_client.py
    install -Dm0644 ${S}/deploy/power_client.py ${D}${libexecdir}/power_client.py
    for unit in hoki-health-recording hoki-health-policy hoki-health-profile-recording; do
        install -Dm0644 ${S}/deploy/$unit.service ${D}${systemd_system_unitdir}/$unit.service
    done
    install -Dm0644 ${S}/deploy/30-hoki-health-recording.rules ${D}${datadir}/polkit-1/rules.d/30-hoki-health-recording.rules
    install -Dm0644 ${S}/README.md ${D}${datadir}/hoki-health-recorder/README.md
    install -Dm0644 ${S}/CAPABILITIES.md ${D}${datadir}/hoki-health-recorder/CAPABILITIES.md
}
FILES:${PN}-ssc = "${libexecdir}/hoki-ssc-recorder"
FILES:${PN} += "${libexecdir}/hoki-manual-consumer ${libexecdir}/health_broker.py ${libexecdir}/health_client.py ${libexecdir}/hoki-recording-suspend-loop ${libexecdir}/hoki-recording-session ${libexecdir}/hoki-health-policy ${libexecdir}/power_client.py ${systemd_system_unitdir}/hoki-health-policy.service ${systemd_system_unitdir}/hoki-health-profile-recording.service ${systemd_system_unitdir}/hoki-health-recording.service ${datadir}/polkit-1/rules.d/30-hoki-health-recording.rules ${datadir}/hoki-health-recorder"
# Profile supervisor runs at boot but default profile is off. It starts only
# the owned profile recording unit when explicitly selected in Settings.
