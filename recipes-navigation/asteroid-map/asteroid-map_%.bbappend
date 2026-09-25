# The map's PositionSource must resolve to the GeoClue v0 QtPositioning plugin
# so GPS requests reach geoclue-provider-hybris on Hoki.
RDEPENDS:${PN}:append:hoki = " qtpositioning-qmlplugins qtpositioning-geoclue qtsensors-qmlplugins nemo-qml-plugin-configuration"

# The community-layer recipe still pins the pre-Qt6 source revision even
# though it inherits qt6-cmake. Pin the current upstream Qt6 port until the
# community recipe catches up.
SRCREV = "8263b2175843e83c32baee2eec5380559db2b735"

# The Qt6 port installs an invoker module in libdir rather than an executable
# binary in bindir. The older community recipe does not include it in FILES.
FILES:${PN}:append = " ${libdir}/${PN}.so"
