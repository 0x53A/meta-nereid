SUMMARY = "Configurable Bluetooth SSH tunnel for Hoki"
LICENSE = "CLOSED"
PR = "r1"
COMPATIBLE_MACHINE = "^hoki$"
PACKAGE_ARCH = "${MACHINE_ARCH}"
FILESEXTRAPATHS:prepend := "${THISDIR}/../../projects:"
require ble-sources.inc
S = "${UNPACKDIR}/projects/ble-ssh/watch-rs"
inherit cargo pkgconfig systemd nereid-cargo-sbom

# Override the manifest's crates.io Git patches directly. Patching their Git
# URLs a second time still makes Cargo try an offline Git checkout.
python ble_fix_git_paths() {
    path = d.expand("${CARGO_HOME}/config.toml")
    with open(path) as f:
        config = f.read().split('\n[patch.', 1)[0]
    config += '\n[patch.crates-io]\n'
    for name in ("bluer", "dbus-crossroads"):
        source = d.expand("${UNPACKDIR}/git-deps/") + name + '/' + name
        config += name + ' = { path = "' + source + '" }\n'
    with open(path, "w") as f:
        f.write(config)
}
do_configure[postfuncs] += "ble_fix_git_paths"

SYSTEMD_SERVICE:${PN} = "ble-ssh-watch.service"
SYSTEMD_AUTO_ENABLE = "disable"
DEPENDS += "dbus"
RDEPENDS:${PN} += "bluez5 dbus dbus-lib"
# The image's existing SSH server supplies authentication and localhost:22.
CONFFILES:${PN} += "${sysconfdir}/default/ble-ssh-watch"
INSANE_SKIP:${PN} += "already-stripped"
INHIBIT_PACKAGE_STRIP = "1"
INHIBIT_SYSROOT_STRIP = "1"

do_install() {
    install -Dm0755 ${B}/target/${CARGO_TARGET_SUBDIR}/ble-ssh-watch ${D}${bindir}/ble-ssh-watch
    install -Dm0644 ${S}/ble-ssh-watch.service ${D}${systemd_system_unitdir}/ble-ssh-watch.service
    install -Dm0644 ${S}/ble-ssh-watch.env ${D}${sysconfdir}/default/ble-ssh-watch
    install -Dm0644 ${S}/com.ble_ssh.conf ${D}${sysconfdir}/dbus-1/system.d/com.ble_ssh.conf
}
FILES:${PN} += "${systemd_system_unitdir}/ble-ssh-watch.service ${datadir}/ble-ssh"
