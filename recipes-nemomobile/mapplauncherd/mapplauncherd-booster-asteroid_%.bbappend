# Dependencies cannot be removed by a systemd drop-in. Adjust the installed
# vendor unit in this optional UI layer, leaving the shared Asteroid layer alone.
do_install:append:hoki() {
    # The layer can also be enabled just for Bluetooth SSH on the stock UI.
    if [ "${@d.getVar('HOKI_CUSTOM_UI') or '1'}" = "1" ]; then
        sed -i 's/asteroid-launcher.service/nereid-compositor.service/g' \
            ${D}${systemd_user_unitdir}/booster-asteroid-qt6.service
    fi
}
