# Personal UI and companion policy; preserve the upstream sensor stack.
HOKI_CUSTOM_UI ?= "1"
HOKI_BLE_SSH ?= "1"
IMAGE_INSTALL:append:hoki = " ${@'hoki-ui packagegroup-hoki-apps qtwayland-plugins tailscale iio-tools' if d.getVar('HOKI_CUSTOM_UI') == '1' else ''}"
IMAGE_INSTALL:append:hoki = " ${@'hoki-health-recorder' if d.getVar('HOKI_CUSTOM_UI') == '1' else ''}"
# hoki-nfc owns tag polling/data exchange. Installing neard as well would race
# the app and its postinstall tries to enable a deliberately masked service.
# The custom phone companion replaces AsteroidOSSync/asteroid-btsyncd.
IMAGE_INSTALL:remove:hoki = "${@'neard asteroid-btsyncd' if d.getVar('HOKI_CUSTOM_UI') == '1' else ''}"
IMAGE_INSTALL:append:hoki = " ${@'ble-ssh-watch' if d.getVar('HOKI_BLE_SSH') == '1' else ''}"
# Ship the diagnostic GPS client and the optional map UI in the Hoki image.
IMAGE_INSTALL:append:hoki = " asteroid-gps-test asteroid-map"

# Include the experimental acoustic transport, with both user services disabled.
HOKI_ACOUSTIC_SSH ?= "1"
IMAGE_INSTALL:append:hoki = " ${@'acoustic-link' if d.getVar('HOKI_ACOUSTIC_SSH') == '1' else ''}"
# Versioned rootfs management; inactive until userdata/.hoki is provisioned.
IMAGE_INSTALL:append:hoki = " hoki-rootfs"

# SSH shell, modern terminal descriptions and the retained diagnostic toolkit.
IMAGE_INSTALL:append:hoki = " packagegroup-nereid-cli"

# Root is the interactive SSH account; service user shells stay unchanged.
inherit extrausers
EXTRA_USERS_PARAMS:append:hoki = " usermod -s /usr/bin/fish root;"
