SUMMARY = "Linux IIO device listing, buffer capture and event tools"
DESCRIPTION = "Small command-line diagnostics for Linux Industrial I/O devices."
HOMEPAGE = "https://github.com/fossil-engineering/kernel-msm-fossil-cw"
LICENSE = "GPL-2.0-only"
LIC_FILES_CHKSUM = "file://COPYING;md5=d7810fab7487fb0aad327b76f1be7cd7"

# Same upstream revision as the Hoki kernel; no local kernel checkout is needed.
SRC_URI = "git://github.com/fossil-engineering/kernel-msm-fossil-cw;protocol=https;branch=fossil-android-msm-hoki-lw1.2-4.14"
SRCREV = "c0b4c201f2d5a641defe19958a9b4c16f40d866b"

# Keep the private UAPI include directory separate from kernel-internal headers.
B = "${WORKDIR}/build"

# Build just the standalone userspace tools, without configuring a kernel.
do_compile() {
    install -d ${B}/include/linux/iio
    install -m 0644 ${S}/include/uapi/linux/iio/events.h ${B}/include/linux/iio/
    install -m 0644 ${S}/include/uapi/linux/iio/types.h ${B}/include/linux/iio/
    ${CC} ${CPPFLAGS} ${CFLAGS} -D_GNU_SOURCE -I${B}/include \
        -c ${S}/tools/iio/iio_utils.c -o ${B}/iio_utils.o
    for tool in lsiio iio_generic_buffer iio_event_monitor; do
        ${CC} ${CPPFLAGS} ${CFLAGS} -D_GNU_SOURCE -I${B}/include \
            ${S}/tools/iio/$tool.c ${B}/iio_utils.o ${LDFLAGS} -o ${B}/$tool
    done
}

do_install() {
    for tool in lsiio iio_generic_buffer iio_event_monitor; do
        install -Dm0755 ${B}/$tool ${D}${bindir}/$tool
    done
}
