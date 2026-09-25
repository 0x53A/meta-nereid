SUMMARY = "Nereid interactive shell and watch diagnostics"
LICENSE = "MIT"
inherit packagegroup

RDEPENDS:${PN} = "\
    fish nereid-terminfo ncurses-tools \
    htop rsync curl strace gdbserver iw ldd elfutils elfutils-binutils \
    tcpdump lsof socat jq evtest fbgrab memtester devmem2 vim-xxd i2c-tools iio-tools \
    util-linux-lsfd util-linux-lscpu util-linux-nsenter util-linux-taskset \
    util-linux-ionice util-linux-chrt util-linux-prlimit util-linux-irqtop \
    util-linux-lsirq util-linux-lsns util-linux-fallocate util-linux-fstrim \
    util-linux-lslocks util-linux-fincore util-linux-unshare \
"
