# Apps share a source-building Cargo recipe, but have separate package ownership
# and runtime dependencies. Keep these names in sync with runtime-projects.txt.
HOKI_APP_PACKAGES = "hoki-clock hoki-activity hoki-haptics hoki-spo2 pebble-runner imu-test-app hoki-audio hoki-music hoki-audiobook hoki-podcast bt-pair hoki-nfc hoki-egui-demo demo-asteroid-app hoki-wasm-host hoki-connect-ui hoki-argyroneta hoki-home-assistant"
PACKAGES =+ "${HOKI_APP_PACKAGES}"
FILES:hoki-activity += "${libexecdir}/hoki-activity ${datadir}/hoki-activity ${systemd_user_unitdir}/hoki-activity.service"
RDEPENDS:hoki-activity += "hoki-health-recorder geoclue python3-core python3-json python3-fcntl python3-io python3-netclient python3-math python3-datetime"
HOKI_APP_PACKAGES:remove = "${@'hoki-wasm-host' if d.getVar('HOKI_WASM_DEMO') != '1' else ''}"
DEPENDS += "gstreamer1.0 glib-2.0 alsa-lib openssl pulseaudio"

python __anonymous() {
    for app in d.getVar('HOKI_APP_PACKAGES').split():
        # Preserve explicit app assets declared below: anonymous Python runs
        # after ordinary += assignments have been evaluated.
        d.appendVar('FILES:' + app, ' /usr/lib/' + app + ' /usr/bin/' + app +
                 ' /usr/share/applications/' + app + '.desktop')
        d.appendVar('RDEPENDS:' + app, ' wayland libxkbcommon fontconfig freetype')
        if app != 'hoki-activity':
            d.appendVar('RDEPENDS:' + app, ' mapplauncherd')
        d.appendVar('INSANE_SKIP:' + app, ' already-stripped')
    d.appendVar('FILES:hoki-nfc', ' ${sysconfdir}/systemd/system/neard.service ${sysconfdir}/systemd/system/dbus-org.neard.service ${sysconfdir}/systemd/system/nfcd.service ${sysconfdir}/systemd/system/nfc-power-off.service')
    d.appendVar('FILES:hoki-argyroneta', ' ${sysconfdir}/dbus-1/system.d/org.hoki.assistant.conf')
}

RDEPENDS:hoki-spo2 += "sensorfw hoki-health-recorder"
FILES:hoki-spo2 += "${datadir}/icons/hicolor/scalable/apps/hoki-spo2.svg"
RDEPENDS:pebble-runner += "sensorfw"
# These two apps used to be files owned by the monolithic hoki-ui package.
RREPLACES:hoki-spo2 = "hoki-ui (<= 1.0-r0)"
RREPLACES:pebble-runner = "hoki-ui (<= 1.0-r0)"
RDEPENDS:imu-test-app += "sensorfw"
RDEPENDS:bt-pair += "bluez5"
RDEPENDS:hoki-audio += "pulseaudio-server pulseaudio-misc pulseaudio-module-raop-sink avahi-utils avahi-daemon"
RDEPENDS:hoki-podcast += "alsa-lib ca-certificates"
RDEPENDS:hoki-connect-ui += "hoki-ui"
# playbin and codecs are loaded dynamically, not found by ELF shlib scanning.
RDEPENDS:hoki-audiobook += "gstreamer1.0 \
    gstreamer1.0-plugins-base-playback gstreamer1.0-plugins-base-typefindfunctions \
    gstreamer1.0-plugins-base-alsa gstreamer1.0-plugins-base-audioconvert \
    gstreamer1.0-plugins-base-audioresample gstreamer1.0-plugins-base-ogg \
    gstreamer1.0-plugins-base-opus gstreamer1.0-plugins-base-vorbis \
    gstreamer1.0-plugins-good-audioparsers gstreamer1.0-plugins-good-isomp4 \
    gstreamer1.0-plugins-good-mpg123 gstreamer1.0-plugins-good-flac \
    gstreamer1.0-plugins-bad-opusparse gstreamer1.0-plugins-bad-faad"

# Direct netlink NFC control requires CAP_NET_ADMIN. Keep the UI as ceres.
RDEPENDS:hoki-nfc += "libcap-bin"
pkg_postinst_ontarget:hoki-nfc() {
    setcap cap_net_admin=ep /usr/lib/hoki-nfc
}

# Music uses system TLS by default; shlib scanning maps libssl/libcrypto/libpulse.
RDEPENDS:hoki-music += "pulseaudio-server ca-certificates"
FILES:hoki-music += "${systemd_user_unitdir}/hoki-music.service"

RDEPENDS:hoki-argyroneta += "hoki-ui bluez5 dbus pulseaudio-server pulseaudio-misc"
RDEPENDS:hoki-home-assistant += "ca-certificates pulseaudio-server pulseaudio-misc"

RDEPENDS:hoki-clock += "hoki-ui"
