SUMMARY = "Local Hoki compositor and Rust shell, retaining Asteroid Qt applications"
LICENSE = "CLOSED"
PR = "r7"
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"
FILESEXTRAPATHS:prepend := "${THISDIR}/../..:${THISDIR}/../../projects:"
SRC_URI = "file://runtime-projects.txt file://nereid-compositor.service file://hoki-hwc-proxy.service"
require hoki-sources.inc
require hoki-apps.inc
S = "${UNPACKDIR}/projects"
CARGO_SRC_DIR = "."
inherit cargo pkgconfig systemd nereid-cargo-sbom
DEPENDS += "dbus fontconfig freetype wayland libxkbcommon libinput udev openssl alsa-lib pulseaudio gstreamer1.0 glib-2.0"
DEPENDS += "libsodium"
export SODIUM_USE_PKG_CONFIG = "1"
DEPENDS += "${@'hoki-wasm-guest-native' if d.getVar('HOKI_WASM_DEMO') == '1' else ''}"
# Several workspace binaries can link concurrently; bound peak LTO memory use.
export CARGO_BUILD_JOBS = "${@min(6, int(oe.utils.parallel_make(d, False) or 1))}"
# Use the target sysroot OpenSSL even when a standalone app enables vendoring.
export OPENSSL_NO_VENDOR = "1"
# One workspace invocation shares resolved versions, features and compilation.
do_compile() {
    export RUSTFLAGS="${RUSTFLAGS}"
    if [ "${HOKI_WASM_DEMO}" = 1 ]; then
        install -m0644 ${STAGING_DATADIR_NATIVE}/hoki-wasm/guest.wasm ${S}/hoki-wasm-host/guest.wasm
    fi
    set --
    if [ "${HOKI_WASM_DEMO}" != 1 ]; then set -- --exclude hoki-wasm-host; fi
    cargo build -v --frozen --release -Z sbom --target ${RUST_HOST_SYS} \
        --manifest-path=${S}/Cargo.toml --workspace --bins "$@"
    ${CC} ${CFLAGS} ${LDFLAGS} -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror \
        -I${S}/nereid-auth/src/native/uapi ${S}/nereid-auth/src/native/rpmb-listener.c -o ${B}/nereid-rpmb-listener
    ${CC} ${CFLAGS} ${LDFLAGS} -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror \
        -I${S}/nereid-auth/src/native/uapi ${S}/nereid-auth/src/native/gatekeeper-backend.c -o ${B}/nereid-gatekeeper-backend
}

SYSTEMD_SERVICE:${PN} = "hoki-hwc-proxy.service hoki-powerd.service hoki-radiod.service hoki-rsb-enable.service hoki-clockd.service"
SYSTEMD_AUTO_ENABLE = "enable"
# Libraries loaded with dlopen are not inferred by shlib dependency scanning.
RDEPENDS:${PN} += "systemd polkit sensorfw mapplauncherd libhybris libinput libudev libxkbcommon wayland fontconfig freetype dbus systemd qtwayland-plugins"
# The compositor starts this service explicitly to keep Asteroid Qt apps usable.
RDEPENDS:${PN} += "mapplauncherd-booster-asteroid"
RDEPENDS:${PN} += "openssh-sftp-server"
RDEPENDS:${PN} += "python3-core python3-threading python3-fcntl psmisc"
RDEPENDS:${PN} += "cryptsetup e2fsprogs-mke2fs util-linux-mount util-linux-umount"
# Cargo release profiles strip their binaries themselves.
INSANE_SKIP:${PN} += "already-stripped"
INHIBIT_PACKAGE_STRIP = "1"
INHIBIT_PACKAGE_DEBUG_SPLIT = "1"

do_install() {
    install -d ${D}/usr/share/hoki
    while IFS='|' read -r source binary destination; do
        case "$source" in ''|\#*) continue ;; esac
        if [ "$source" = hoki-wasm-host ] && [ "${HOKI_WASM_DEMO}" != 1 ]; then continue; fi
        install -Dm0755 ${B}/target/${CARGO_TARGET_SUBDIR}/$binary ${D}/$destination/$binary
        if [ -f ${S}/$source/deploy/$binary.desktop ]; then
            install -Dm0644 ${S}/$source/deploy/$binary.desktop ${D}${datadir}/applications/$binary.desktop
            launcher=${S}/$source/deploy/$binary
            [ -f "$launcher" ] || launcher="$launcher.sh"
            install -Dm0755 "$launcher" ${D}${bindir}/$binary
        fi
    done < ${UNPACKDIR}/runtime-projects.txt
    install -d ${D}${libexecdir}/nereid-auth
    install -m0755 ${B}/nereid-rpmb-listener ${D}${libexecdir}/nereid-auth/rpmb-listener
    install -m0755 ${B}/nereid-gatekeeper-backend ${D}${libexecdir}/nereid-auth/nereid-gatekeeper-backend
    install -m0755 ${S}/nereid-auth/src/native/backend.py ${D}${libexecdir}/nereid-auth/backend.py
    install -m0644 ${S}/nereid-auth/src/native/supervisor.py ${D}${libexecdir}/nereid-auth/supervisor.py
    # Installed but deliberately not auto-enabled until device validation.
    install -Dm0644 ${S}/nereid-auth/deploy/nereid-auth.service ${D}${systemd_system_unitdir}/nereid-auth.service
    install -Dm0644 ${S}/nereid-auth/deploy/io.Nereid.Auth1.conf ${D}${sysconfdir}/dbus-1/system.d/io.Nereid.Auth1.conf
    install -Dm0644 ${S}/nereid-auth/deploy/keymaster.conf.hoki-reference ${D}${datadir}/nereid-auth/keymaster.conf.hoki-reference
    install -d ${D}${libexecdir}/hoki-activity ${D}${datadir}/hoki-activity
    install -m0644 ${S}/hoki-activity/daemon.py ${S}/hoki-activity/activity.py ${S}/hoki-health-recorder/deploy/health_client.py ${S}/hoki-health-recorder/deploy/power_client.py ${D}${libexecdir}/hoki-activity/
    install -m0644 ${S}/hoki-activity/README.md ${S}/hoki-activity/export.py ${D}${datadir}/hoki-activity/
    install -Dm0644 ${S}/hoki-activity/deploy/hoki-activity.service ${D}${systemd_user_unitdir}/hoki-activity.service
    install -Dm0644 ${S}/hoki-spo2/deploy/hoki-spo2.svg ${D}${datadir}/icons/hicolor/scalable/apps/hoki-spo2.svg
    install -Dm0644 ${S}/hoki-clock/deploy/hoki-clockd.service ${D}${systemd_system_unitdir}/hoki-clockd.service
    install -Dm0644 ${S}/hoki-assistant/deploy/org.hoki.assistant.conf ${D}${sysconfdir}/dbus-1/system.d/org.hoki.assistant.conf
    install -Dm0644 ${S}/hoki-powerd/deploy/suspend-gate.conf ${D}${systemd_system_unitdir}/systemd-suspend.service.d/50-hoki-powerd.conf
    install -Dm0644 ${S}/hoki-powerd/deploy/30-hoki-inhibitors.rules ${D}${datadir}/polkit-1/rules.d/30-hoki-inhibitors.rules
    for project in hoki-powerd hoki-radiod; do
        install -Dm0644 ${S}/$project/deploy/$project.service ${D}${systemd_system_unitdir}/$project.service
        install -d ${D}${sysconfdir}/dbus-1/system.d ${D}${datadir}/dbus-1/system-services
        install -m0644 ${S}/$project/deploy/org.hoki.*.conf ${D}${sysconfdir}/dbus-1/system.d/
        install -m0644 ${S}/$project/deploy/org.hoki.*.service ${D}${datadir}/dbus-1/system-services/
    done
    for face in hoki-digital hoki-seconds hoki-orbit hoki-instrument; do
        install -Dm0644 ${S}/hoki-lp-watchface/deploy/$face.json ${D}${datadir}/hoki/ambient-faces/$face.json
    done
    install -Dm0644 ${S}/nereid-compositor/opk/hoki-rsb-enable.service ${D}${systemd_system_unitdir}/hoki-rsb-enable.service
    for project in hoki-connect hoki-music; do
        install -Dm0644 ${S}/$project/deploy/$project.service ${D}${systemd_user_unitdir}/$project.service
    done
    (cd ${D} && find usr/local/bin usr/lib -maxdepth 1 -type f -print0 | sort -z | xargs -0 sha256sum > usr/share/hoki/runtime-sha256.txt)
    install -Dm0644 ${UNPACKDIR}/nereid-compositor.service ${D}${systemd_user_unitdir}/nereid-compositor.service
    install -Dm0644 ${UNPACKDIR}/hoki-hwc-proxy.service ${D}${systemd_system_unitdir}/hoki-hwc-proxy.service
    install -d ${D}${sysconfdir}/systemd/user/default.target.wants
    ln -s /dev/null ${D}${sysconfdir}/systemd/user/asteroid-launcher.service
    ln -s ${systemd_user_unitdir}/nereid-compositor.service ${D}${sysconfdir}/systemd/user/default.target.wants/nereid-compositor.service
    # Start the Connect client only when a personalized peer configuration exists.
    ln -s ${systemd_user_unitdir}/hoki-connect.service ${D}${sysconfdir}/systemd/user/default.target.wants/hoki-connect.service
    install -d ${D}${sysconfdir}/systemd/system
    # The NFC app owns kernel polling/data exchange directly. neard would
    # claim and deactivate its tags; block both ordinary and D-Bus activation.
    ln -s /dev/null ${D}${sysconfdir}/systemd/system/neard.service
    ln -s /dev/null ${D}${sysconfdir}/systemd/system/dbus-org.neard.service
    ln -s /dev/null ${D}${sysconfdir}/systemd/system/nfcd.service
    ln -s /dev/null ${D}${sysconfdir}/systemd/system/nfc-power-off.service
}
FILES:${PN} += "${datadir}/polkit-1/rules.d/30-hoki-inhibitors.rules ${systemd_system_unitdir}/systemd-suspend.service.d /usr/local /usr/lib/hoki-* /usr/lib/pebble-runner ${systemd_user_unitdir} /usr/share/hoki /etc/systemd/user ${datadir}/dbus-1/system-services"
FILES:${PN} += "${libexecdir}/nereid-auth ${libexecdir}/nereid-authd ${systemd_system_unitdir}/nereid-auth.service"
FILES:${PN} += "${datadir}/nereid-auth"

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
After=dbus.socket nereid-compositor.service
Wants=nereid-compositor.service
PartOf=nereid-compositor.service
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
