# Audiobook Player — Build Dependencies

## Intended production build (not yet implemented)

Give `hoki-audiobook` its own source-building BitBake recipe in
`meta-nereid`, using Yocto's Cargo integration and dependencies pinned from
`Cargo.lock`. Declare GStreamer, GLib and the Slint graphics dependencies in
`DEPENDS` so compilation links against real target libraries in the recipe
sysroot. Package the binary and launcher, and retain explicit runtime codec
plugin dependencies from `hoki-apps.inc` (including the existing AAC policy).

Once that recipe is validated, remove the audiobook from the external runtime
bundle and its package split to avoid duplicate file ownership. The production
build should no longer need stub `.pc`/`.so` files, an external Nix cross-build,
or post-build loader/RPATH patching. Nix can remain useful for desktop development.
The new link-stub generator only preserves the current workflow; it is not the
intended production architecture. This migration is deferred.

Currently BitBake packages the externally built app from `hoki-runtime.tar.gz`.
See [the current app integration](../../APPS.md). The manual library
build/deployment notes below are historical; image-level ALSA patches and codec
packages now belong to their existing layer recipes.

## Historical manual dependencies

Two custom-built shared libraries are needed on the watch for audio playback.

## 1. Patched alsa-lib (libasound.so.2.0.0)

**Problem**: Qualcomm ASoC PCM driver returns ENOTTY for `SNDRV_PCM_IOCTL_SYNC_PTR`
before `hw_params` are set. alsa-lib 1.2.13 treats this as fatal during
`snd_pcm_hw_open()`, so PulseAudio and GStreamer can't open the ALSA device.

**Source**: alsa-lib 1.2.13 — https://www.alsa-project.org/files/pub/lib/alsa-lib-1.2.13.tar.bz2

**Patch**: `alsa-lib-patch/alsa-lib-sync-ptr-tolerate.patch`
- One change in `src/pcm/pcm_hw.c` — makes initial `sync_ptr1()` failure non-fatal

**Build layout**:
```
alsa-lib-patch/
├── alsa-lib-1.2.13.tar.bz2      # upstream tarball (1.1 MB)
├── alsa-lib-1.2.13/              # extracted, patch applied
├── alsa-lib-sync-ptr-tolerate.patch
├── build.sh                      # cross-compile script
└── install/lib/libasound.so.2.0.0  # output (~750 KB)
```

**Build**: `nix-shell --run ./build.sh` (uses the hoki-audiobook shell.nix)

**Deploy**: `scp install/lib/libasound.so.2.0.0 root@hoki.local:/usr/lib/libasound.so.2.0.0`

**Not packaged as opk** — deployed as a direct library replacement on the watch.

---

## 2. GStreamer Opus plugin (libgstopus.so + libopus.so)

**Problem**: Watch has GStreamer 1.24.13 but no Opus codec support. Audiobooks
are encoded as Opus for best quality-per-bit at low bitrates.

**Sources** (all unmodified upstream tarballs):
- gst-plugins-base-1.24.13 — https://gstreamer.freedesktop.org/src/gst-plugins-base/gst-plugins-base-1.24.13.tar.xz (2.4 MB)
- gst-plugins-bad-1.24.13 — https://gstreamer.freedesktop.org/src/gst-plugins-bad/gst-plugins-bad-1.24.13.tar.xz (6.8 MB)
- gstreamer-1.24.13 — https://gstreamer.freedesktop.org/src/gstreamer/gstreamer-1.24.13.tar.xz (1.8 MB)
- libopus 1.5.2 — from nixpkgs (`pkgsCross.armv7l-hf-multiplatform`)

Only the opus plugin sources from `gst-plugins-base-1.24.13/ext/opus/` are compiled.
The other tarballs provide headers only. No source modifications.

**Build layout**:
```
gst-opus-build/
├── gstreamer-1.24.13.tar.xz         # upstream (headers only)
├── gstreamer-1.24.13/
├── gst-plugins-base-1.24.13.tar.xz  # upstream (opus source + headers)
├── gst-plugins-base-1.24.13/
├── gst-plugins-bad-1.24.13.tar.xz   # upstream (headers only)
├── gst-plugins-bad-1.24.13/
├── config.h                          # minimal config for cross-compile
├── include/                          # header shims (gst, glib, opus)
├── build.sh                          # cross-compile script
├── build-package.sh                  # opkg packaging script
├── out/
│   ├── libgstopus.so                 # GStreamer plugin (~77 KB)
│   ├── libopus.so.0.10.1             # opus codec lib (~410 KB)
│   └── libopus.so.0 → libopus.so.0.10.1
└── 0x53a.gst-opus_1.24.13_armv7vehf-neon.opk  # packaged (268 KB)
```

**Build**: `nix-shell --run ./build.sh && ./build-package.sh`

**Deploy**: Install the opk on the watch:
```sh
scp 0x53a.gst-opus_1.24.13_armv7vehf-neon.opk root@hoki.local:/tmp/
ssh root@hoki.local 'opkg install /tmp/0x53a.gst-opus_1.24.13_armv7vehf-neon.opk'
```

Installs `libopus.so` to `/usr/lib/` and `libgstopus.so` to `/usr/lib/gstreamer-1.0/`.
