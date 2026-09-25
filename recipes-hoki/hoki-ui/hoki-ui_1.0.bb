SUMMARY = "Local Hoki compositor and Rust shell, retaining Asteroid Qt applications"
LICENSE = "CLOSED"
PR = "r3"
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"
SRC_URI = "file://hoki-runtime.tar.gz file://asteroid-compositor.service file://hoki-hwc-proxy.service"
require hoki-apps.inc
S = "${UNPACKDIR}"
inherit systemd
SYSTEMD_SERVICE:${PN} = "hoki-hwc-proxy.service hoki-powerd.service hoki-radiod.service hoki-rsb-enable.service"
SYSTEMD_AUTO_ENABLE = "enable"
# Libraries loaded with dlopen are not inferred by shlib dependency scanning.
RDEPENDS:${PN} += "sensorfw mapplauncherd libhybris libinput libudev libxkbcommon wayland fontconfig freetype dbus systemd qtwayland-plugins"
# The compositor starts this service explicitly to keep Asteroid Qt apps usable.
RDEPENDS:${PN} += "mapplauncherd-booster-asteroid"
# Most Cargo release artifacts are already stripped by their profiles.
INSANE_SKIP:${PN} += "already-stripped"
INHIBIT_PACKAGE_STRIP = "1"
INHIBIT_SYSROOT_STRIP = "1"
# Preserve the prebuilt bytes recorded in runtime-sha256.txt, including the
# Pebble runner's symbol/debug metadata (its profile does not strip everything).
INHIBIT_PACKAGE_DEBUG_SPLIT = "1"

do_install() {
    cp -R --no-preserve=ownership ${UNPACKDIR}/hoki-runtime/. ${D}/
    install -Dm0644 ${UNPACKDIR}/asteroid-compositor.service ${D}${systemd_user_unitdir}/asteroid-compositor.service
    install -Dm0644 ${UNPACKDIR}/hoki-hwc-proxy.service ${D}${systemd_system_unitdir}/hoki-hwc-proxy.service
    install -d ${D}${sysconfdir}/systemd/user/default.target.wants
    ln -s /dev/null ${D}${sysconfdir}/systemd/user/asteroid-launcher.service
    ln -s ${systemd_user_unitdir}/asteroid-compositor.service ${D}${sysconfdir}/systemd/user/default.target.wants/asteroid-compositor.service
    # Start the Connect client only when a personalized peer configuration exists.
    ln -s ${systemd_user_unitdir}/hoki-connect.service ${D}${sysconfdir}/systemd/user/default.target.wants/hoki-connect.service
    # MCE's hybris framebuffer/backlight policy conflicts with our HWC proxy
    # and compositor, which own display power and input-driven blanking.
    install -d ${D}${sysconfdir}/systemd/system
    ln -s /dev/null ${D}${sysconfdir}/systemd/system/mce.service
    # The NFC app owns kernel polling/data exchange directly. neard would
    # claim and deactivate its tags; block both ordinary and D-Bus activation.
    ln -s /dev/null ${D}${sysconfdir}/systemd/system/neard.service
    ln -s /dev/null ${D}${sysconfdir}/systemd/system/dbus-org.neard.service
    ln -s /dev/null ${D}${sysconfdir}/systemd/system/nfcd.service
    ln -s /dev/null ${D}${sysconfdir}/systemd/system/nfc-power-off.service
}
FILES:${PN} += "/usr/local /usr/lib/hoki-* /usr/lib/pebble-runner ${systemd_user_unitdir} /usr/share/hoki /etc/systemd/user /etc/systemd/system/mce.service ${datadir}/dbus-1/system-services"

# Boosted Qt applications run in a prestarted process, so their environment
# must be configured on that service, not only on the invoker client.
do_install:append() {
    cat > ${D}/usr/share/hoki/qt-wayland.env <<'ENV'
QT_QPA_PLATFORM=wayland
QT_QUICK_BACKEND=software
QT_WAYLAND_CLIENT_BUFFER_INTEGRATION=none
QT_WAYLAND_DISABLE_WINDOWDECORATION=1
WAYLAND_DISPLAY=wayland-0
XDG_RUNTIME_DIR=/run/user/1000
ENV
    for unit in booster-asteroid-qt6 booster-qt6 booster-generic; do
        install -d ${D}${sysconfdir}/systemd/user/$unit.service.d
        cat > ${D}${sysconfdir}/systemd/user/$unit.service.d/hoki-wayland.conf <<'ENV'
[Unit]
After=dbus.socket asteroid-compositor.service
Wants=asteroid-compositor.service
PartOf=asteroid-compositor.service
[Service]
# Later EnvironmentFile entries override the stock wayland-egl selection;
# Environment= alone cannot override a value from EnvironmentFile=.
EnvironmentFile=/usr/share/hoki/qt-wayland.env
ExecStartPre=/bin/sh -c 'for i in $(seq 30); do [ -S /run/user/1000/wayland-0 ] && exit 0; sleep 1; done; exit 1'
Restart=always
RestartSec=2
ENV
    done
}
