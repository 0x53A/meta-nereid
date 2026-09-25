#!/bin/bash
set -euo pipefail

# Cross-compile GStreamer opus plugin (opusdec/opusenc) for armv7hf
#
# Fetches all headers and libraries from nix ARM cross packages at build time.
# The watch runs GStreamer 1.24.13, but nix ships 1.26.x — we patch
# gstversion.h to report 1.24.13 so the plugin is accepted by the runtime.
# The API surface used by the opus plugin is stable across both versions.
#
# Source: upstream gst-plugins-base 1.24.13 tarball (ext/opus/)
#
# Prerequisites:
#   - nix with pkgsCross.armv7l-hf-multiplatform available
#   - gst-plugins-base-1.24.13 tarball extracted in this directory
#   - If gnutls cross-build is broken, apply the fix from
#     https://github.com/NixOS/nixpkgs/pull/499659 or use:
#       -I nixpkgs=/path/to/patched/nixpkgs

BUILDDIR="$(cd "$(dirname "$0")" && pwd)"
SRCDIR="$BUILDDIR/gst-plugins-base-1.24.13/ext/opus"
OUTDIR="$BUILDDIR/out"
INCDIR="$BUILDDIR/include"

# Version the watch actually runs
WATCH_GST_VERSION_MINOR=24
WATCH_GST_VERSION_MICRO=13

NIX_ARGS=("--no-out-link")
# Allow passing extra nix args, e.g. -I nixpkgs=/tmp/nixpkgs-fix
NIX_ARGS+=("$@")

if [ ! -d "$SRCDIR" ]; then
    echo "ERROR: Source not found at $SRCDIR"
    echo "Download and extract gst-plugins-base-1.24.13.tar.xz first."
    exit 1
fi

mkdir -p "$OUTDIR"

# --- Resolve nix packages ---

echo "==> Resolving ARM cross packages from nix..."

cross='(import <nixpkgs> {}).pkgsCross.armv7l-hf-multiplatform'

GST_DEV=$(nix-build "${NIX_ARGS[@]}" -E "${cross}.gst_all_1.gstreamer.dev")
GST_BASE_DEV=$(nix-build "${NIX_ARGS[@]}" -E "${cross}.gst_all_1.gst-plugins-base.dev")
GLIB_DEV=$(nix-build "${NIX_ARGS[@]}" -E "${cross}.glib.dev")
GLIB_OUT=$(nix-build "${NIX_ARGS[@]}" -E "${cross}.glib.out")
OPUS_DEV=$(nix-build "${NIX_ARGS[@]}" -E "${cross}.libopus.dev")
OPUS_OUT=$(nix-build "${NIX_ARGS[@]}" -E "${cross}.libopus.out")

GST_OUT=$(nix-build "${NIX_ARGS[@]}" -E "${cross}.gst_all_1.gstreamer.out")
GST_BASE_OUT=$(nix-build "${NIX_ARGS[@]}" -E "${cross}.gst_all_1.gst-plugins-base.out")

echo "    gstreamer.dev:        $GST_DEV"
echo "    gst-plugins-base.dev: $GST_BASE_DEV"
echo "    glib.dev:             $GLIB_DEV"
echo "    libopus.dev:          $OPUS_DEV"

# --- Populate include/ from nix packages ---

echo "==> Populating headers..."

rm -rf "$INCDIR"
mkdir -p "$INCDIR/glib-2.0" "$INCDIR/opus"

# GStreamer core headers
cp -r --no-preserve=mode "$GST_DEV/include/gstreamer-1.0/gst" "$INCDIR/gst"

# GStreamer base plugin headers (audio, pbutils, tag, video — video needed by pbutils)
cp -r --no-preserve=mode "$GST_BASE_DEV/include/gstreamer-1.0/gst/audio" "$INCDIR/gst/"
cp -r --no-preserve=mode "$GST_BASE_DEV/include/gstreamer-1.0/gst/pbutils" "$INCDIR/gst/"
cp -r --no-preserve=mode "$GST_BASE_DEV/include/gstreamer-1.0/gst/tag" "$INCDIR/gst/"
cp -r --no-preserve=mode "$GST_BASE_DEV/include/gstreamer-1.0/gst/video" "$INCDIR/gst/"

# glib headers (dev has include/glib-2.0/*, out has lib/glib-2.0/include/glibconfig.h)
cp -r --no-preserve=mode "$GLIB_DEV/include/glib-2.0/"* "$INCDIR/glib-2.0/"
cp --no-preserve=mode "$GLIB_OUT/lib/glib-2.0/include/glibconfig.h" "$INCDIR/glib-2.0/"

# opus headers
cp -r --no-preserve=mode "$OPUS_DEV/include/opus/"* "$INCDIR/opus/"

# Private header needed by the opus plugin source
if [ -f "$BUILDDIR/gstreamer-1.24.13/gst/glib-compat-private.h" ]; then
    cp "$BUILDDIR/gstreamer-1.24.13/gst/glib-compat-private.h" "$INCDIR/gst/"
else
    echo "WARNING: glib-compat-private.h not found in gstreamer-1.24.13 tarball"
    echo "         Download gstreamer-1.24.13.tar.xz if build fails"
fi

# Patch gstversion.h to match the watch runtime version
sed -i "s/GST_VERSION_MINOR ([0-9]*)/GST_VERSION_MINOR ($WATCH_GST_VERSION_MINOR)/" "$INCDIR/gst/gstversion.h"
sed -i "s/GST_VERSION_MICRO ([0-9]*)/GST_VERSION_MICRO ($WATCH_GST_VERSION_MICRO)/" "$INCDIR/gst/gstversion.h"

echo "    $(find "$INCDIR" -type f | wc -l) header files"
echo "    gstversion.h patched to 1.${WATCH_GST_VERSION_MINOR}.${WATCH_GST_VERSION_MICRO}"

# --- Compile ---

CC="${CC_armv7:-armv7l-unknown-linux-gnueabihf-cc}"

SOURCES=(
    "$SRCDIR/gstopus.c"
    "$SRCDIR/gstopuselement.c"
    "$SRCDIR/gstopuscommon.c"
    "$SRCDIR/gstopusdec.c"
    "$SRCDIR/gstopusenc.c"
    "$SRCDIR/gstopusheader.c"
)

CFLAGS=(
    -shared -fPIC -O2
    -DHAVE_CONFIG_H
    -Wall -Wno-deprecated-declarations
    -I"$BUILDDIR"
    -I"$INCDIR"
    -I"$INCDIR/gst"
    -I"$INCDIR/glib-2.0"
    -I"$INCDIR/opus"
)

LDFLAGS=(
    -L"$GST_OUT/lib"
    -L"$GST_BASE_OUT/lib"
    -L"$GLIB_OUT/lib"
    -L"$OPUS_OUT/lib"
    -lgstreamer-1.0
    -lgstbase-1.0
    -lgstaudio-1.0
    -lgstpbutils-1.0
    -lgsttag-1.0
    -lgobject-2.0
    -lglib-2.0
    -lopus
    -lm
)

echo "==> Compiling GStreamer opus plugin for ARM..."
echo "    CC: $CC"
echo "    Sources: ${#SOURCES[@]} files"

$CC "${CFLAGS[@]}" "${SOURCES[@]}" "${LDFLAGS[@]}" -o "$OUTDIR/libgstopus.so"

echo "==> Built: $OUTDIR/libgstopus.so"
file "$OUTDIR/libgstopus.so"

# Fix rpath to find libs on watch
patchelf --set-rpath /usr/lib:/lib "$OUTDIR/libgstopus.so"

echo "==> Patched rpath"

# Also copy libopus
OPUS_SONAME=$(readlink -f "$OPUS_OUT/lib/libopus.so" | xargs basename)
cp "$OPUS_OUT/lib/$OPUS_SONAME" "$OUTDIR/$OPUS_SONAME"
(cd "$OUTDIR" && ln -sf "$OPUS_SONAME" libopus.so.0 && ln -sf libopus.so.0 libopus.so)

echo "==> Copied libopus ($OPUS_SONAME)"
echo ""
echo "Output files:"
ls -la "$OUTDIR/"
