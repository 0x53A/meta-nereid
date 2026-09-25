# The app imports QtPositioning from QML and relies on the GeoClue v0 bridge to
# reach the Hoki GeoClue provider.
FILESEXTRAPATHS:prepend := "${THISDIR}/files:"
SRC_URI += "file://0001-launch-with-asteroid-qt6-booster.patch \
            file://0002-show-gps-acquisition-state.patch \
           "

DEPENDS:remove:hoki = "qtlocation"
DEPENDS:append:hoki = " qtpositioning"
RDEPENDS:${PN}:remove:hoki = "qtlocation"
RDEPENDS:${PN}:append:hoki = " qtpositioning-qmlplugins qtpositioning-geoclue"

# Hoki recorder consumes raw GeoClue data to retain validity flags and all signals.
FILESEXTRAPATHS:prepend := "${THISDIR}/../../projects/hoki-gps-recorder:"
SRC_URI:append:hoki = " file://0003-geoclue-recorder.patch file://geocluerecorder.cpp file://geocluerecorder.h file://main.qml"
PR:append:hoki = ".recorder6"
do_configure:prepend:hoki() {
    install -m 0644 ${UNPACKDIR}/geocluerecorder.cpp ${UNPACKDIR}/geocluerecorder.h ${UNPACKDIR}/main.qml ${S}/src/
}
