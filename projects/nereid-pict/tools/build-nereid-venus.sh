#!/usr/bin/env bash
# Cross-build the opt-in Hoki worker using a relocated Yocto SDK in Docker.
# Required: PICT_WATCH_BUILD=/path/to/hoki/build, PICT_WATCH_ELF_PATCH=/path/to/patch-watch-elf.sh
set -euo pipefail
: "${PICT_WATCH_BUILD:?Set the Hoki build root containing sdk-validation/full}"
: "${PICT_WATCH_ELF_PATCH:?Set the watch ELF patch helper path}"
: "${NEREID_SDK_IMAGE:?Set an immutable SDK image name@sha256:digest}"
[[ "$NEREID_SDK_IMAGE" == *@sha256:* ]] || { echo "NEREID_SDK_IMAGE must use a digest" >&2; exit 1; }
pict_root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$PICT_WATCH_BUILD/pict-venus"
docker run --rm -i --network none --user "$(id -u):$(id -g)" --cpus 4 \
 -v "$PICT_WATCH_BUILD:/work" -v "$pict_root:/src:ro" \
 "$NEREID_SDK_IMAGE" bash -s <<'BUILD'
set -euo pipefail
source /work/sdk-validation/full/environment-setup-armv7vehf-neon-oe-linux-gnueabi
cd /work/pict-venus
for protocol in ext-image-copy-capture ext-image-capture-source ext-foreign-toplevel-list; do
 xml="$OECORE_NATIVE_SYSROOT/usr/share/wayland-protocols/staging/$protocol/$protocol-v1.xml"
 wayland-scanner client-header "$xml" "$protocol-v1-client-protocol.h"
 wayland-scanner private-code "$xml" "$protocol-v1-protocol.c"
done
# The downstream kernel's V4L2 timeval ABI requires native ARM32 time fields.
$CC $CFLAGS -U_TIME_BITS -U_FILE_OFFSET_BITS -O3 -Wall -Wextra -Wno-unused-parameter -I. \
 /src/tools/nereid-venus.c *-protocol.c $LDFLAGS -lwayland-client -o nereid-venus
BUILD
export PICT_WATCH_BUILD PICT_WATCH_ELF_PATCH
nix-shell -p patchelf --run 'bash "$PICT_WATCH_ELF_PATCH" "$PICT_WATCH_BUILD/pict-venus/nereid-venus"'
