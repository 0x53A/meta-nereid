# Nereid command-line tools

`packagegroup-nereid-cli` makes the diagnostic toolkit part of every Hoki image,
so replacing a rootfs no longer loses packages installed manually on the old
version. Existing OpenEmbedded recipes build the utilities from source; Fish
and modern terminal descriptions have recipes in this layer.

Includes Fish 4.5.0, Kitty/Ghostty terminfo, ncurses tools, htop, rsync, curl,
strace, gdbserver, iw, ldd, elfutils, tcpdump, lsof, socat, jq, evtest, fbgrab,
memtester, devmem2, xxd, I2C/IIO tools, and the selected util-linux diagnostics
listed in the recipe. BusyBox already supplies `ifconfig`.

Fish is available as `/usr/bin/fish` and registered in `/etc/shells`. The Nereid
image sets it as root's login shell through `extrausers`; service accounts keep
their existing shells. This takes effect when the image is deployed.
