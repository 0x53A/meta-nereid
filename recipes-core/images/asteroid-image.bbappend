# Personal UI and companion policy; preserve the upstream sensor stack.
HOKI_CUSTOM_UI ?= "1"
HOKI_BLE_SSH ?= "1"
IMAGE_INSTALL:append:hoki = " ${@'hoki-ui packagegroup-hoki-apps qtwayland-plugins tailscale iio-tools' if d.getVar('HOKI_CUSTOM_UI') == '1' else ''}"
IMAGE_INSTALL:append:hoki = " ${@'hoki-health-recorder hoki-activity' if d.getVar('HOKI_CUSTOM_UI') == '1' else ''}"
# hoki-nfc owns tag polling/data exchange. Installing neard as well would race
# the app and its postinstall tries to enable a deliberately masked service.
# The custom phone companion replaces AsteroidOSSync/asteroid-btsyncd.
# The custom compositor/HWC proxy own display transitions, so omit MCE rather
# than installing its daemon and masking its service. Stock images retain MCE.
IMAGE_INSTALL:remove:hoki = "${@'neard asteroid-btsyncd mce' if d.getVar('HOKI_CUSTOM_UI') == '1' else ''}"
PACKAGE_EXCLUDE:append:hoki = " ${@'mce' if d.getVar('HOKI_CUSTOM_UI') == '1' else ''}"
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
# Preserve Yocto's installed-package license manifest and license texts in the
# image. The complete image SPDX is copied beside the managed rootfs by the
# workstation bundle tool after do_image, avoiding a self-referential hash.
COPY_LIC_MANIFEST:hoki = "1"
COPY_LIC_DIRS:hoki = "1"
inherit extrausers
EXTRA_USERS_PARAMS:append:hoki = " usermod -s /usr/bin/fish root;"

# Both images come from the same fakeroot tree, preserving inode metadata/xattrs.
IMAGE_FSTYPES:append:hoki = " squashfs-lz4"
EXTRA_IMAGECMD:squashfs-lz4 = "-b 131072 -processors ${@d.getVar('BB_NUMBER_THREADS') or '2'}"
