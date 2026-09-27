# Custom Hoki apps

`HOKI_CUSTOM_UI = "1"` selects `hoki-ui` and `packagegroup-hoki-apps`.
The custom image omits MCE; Nereid's compositor and HWC proxy own display
transitions. Stock mode retains the upstream MCE package.
BitBake builds the runtime's Rust projects from their source and splits the apps
into the existing individual packages. The runtime shares `projects/Cargo.toml`
and one `projects/Cargo.lock`, fetches checksummed crates during `do_fetch`, and compiles offline
against the target sysroot. Audiobook links real GStreamer/GLib libraries.
Bluetooth SSH and health recording have their own Cargo recipes; SSC uses the
existing pinned `android-ndk-native` toolchain. The WebAssembly demo and its
matching standard library are built by `hoki-wasm-guest-native` when enabled.
The demo is disabled by default (no compilation or installation); set
`HOKI_WASM_DEMO = "1"` in BitBake configuration to enable it again. Its sources
and recipes remain available.

Build from the repository root after configuring the remote builder:

```sh
bash meta-nereid/tools/build-hoki.sh
```

After changing Cargo lockfiles or adding projects/assets, regenerate and review
locked recipe inputs:

```sh
python3 meta-nereid/tools/update-runtime-recipes.py
```

The wrapper rejects stale generated metadata, stages declared source inputs,
and runs the full BitBake image build. Host Nix, local ARM artifacts, stub
libraries and manual ELF patching are not required for image builds. The old
runtime bundle builders remain available for standalone development.

The 24 runtime packages use a Cargo workspace with shared dependency versions,
including Slint/slint-build 1.15.1. BitBake builds selected members in one Cargo
invocation, sharing resolved features and compiled dependencies. Release settings
are centralized; Music, Pebble, armagnac and Symphonia retain speed optimization.
The remaining code uses the common size-optimized profile. BLE SSH and the health
recorder retain their independent recipes/lockfiles, and the WASM guest retains
its separate target build. See [workspace usage](projects/README.md).

Tailscale still uses official prebuilt ARM binaries. Android compatibility
libraries, vendor HALs and firmware are binary inputs; BitBake fetches/packages
them. Personalization with private identities remains a separate local step.
Both stock and custom configurations remain supported; validating a custom
image does not establish stock-image validation.

## App inventory

| Project | Image packaging |
| --- | --- |
| hoki-launcher, hoki-settings, hoki-watchface | Shell in `hoki-ui`; watchface also has its existing desktop entry |
| hoki-spo2 | `hoki-spo2` package, sensorfw |
| pebble-runner | `pebble-runner` package, sensorfw |
| imu-test-app | `imu-test-app` package; new IMU Test desktop entry, sensorfw |
| hoki-audio | `hoki-audio` package; PulseAudio server and pactl |
| hoki-audiobook | `hoki-audiobook` package; playbin, ALSA sink, Ogg/Vorbis/Opus, MP3, FLAC, MP4/AAC plugins |
| hoki-podcast | `hoki-podcast` package; ALSA and certificates; user data under XDG_DATA_HOME or HOME |
| bt-pair | `bt-pair` package; bluetoothctl from bluez5 |
| hoki-nfc | `hoki-nfc` package; direct kernel NFC access; custom image omits neard and masks its service/alias to prevent competing tag ownership |
| hoki-egui-demo | `hoki-egui-demo` package |
| demo-rust-app | `demo-asteroid-app` package (binary differs from directory name) |
| hoki-wasm-host + hoki-wasm-guest | Optional `hoki-wasm-host` package with embedded guest; disabled by default |
| asteroid-compass | Existing patched upstream recipe retained |
| asteroid-health | Existing community recipe selected; pulls `asteroid-sensorlogd` step/heart-rate logger and QML plugin, with a Hoki startup fix |
| asteroid-gps-test, asteroid-map | Existing image selection retained |
| Other stock Asteroid apps | Existing upstream image selection retained |

Every packaged graphical app launches via invoker as the existing `ceres`
session, with `/run/user/1000` and `wayland-0`. No root UI launchers are added.
App package dependencies cover tools and dynamically loaded codecs that ELF
scanning cannot discover. The personal layer accepts the specific
`commercial_faad2` BitBake flag for its AAC decoder; it does not enable all
commercial-flagged recipes.

## Inspected projects not selected as watch apps

| Project | Reason |
| --- | --- |
| hoki-nfc-emul | Empty `src/`, no implementation to compile |
| bt-mode-toggle | Legacy root-only BlueZ configuration editor/restart tool; Settings/ConnMan own current radio controls |
| hoki-emulator, hoki-simulator | Desktop tools |
| hoki-companion | Android phone application |
| hoki-sidekick-probe, gps-test, diag-efs-tool | Standalone hardware/CLI diagnostics, no watch launcher UI; image already has asteroid-gps-test |
| hoki-bt-tunnel | Separate experimental gateway/tunnel service, not a launcher app; Bluetooth SSH has its own recipe |
| ble-ssh | Existing `ble-ssh-watch` recipe and image switch |
| hoki-location | Existing broker recipe/provider integration |
| hoki-powerd, hoki-radiod, hoki-hwc-proxy, nereid-compositor | Existing shell services in `hoki-ui` |
| fish-shell, chunked, alsa-lib-patch, shared | Shell/tooling/library/support code, not graphical apps |

Add complete apps to `runtime-projects.txt`, `hoki-ui/hoki-apps.inc` and the
package group, including their runtime dependencies, then regenerate recipe
inputs. `check-runtime.py` checks the source launcher inventory and legacy
bundles. Packaging
checks do not establish hardware behavior; playback, NFC and physical sensor
motion still need on-watch validation after deployment.

## Acoustic SSH (experimental, installed without autostart)

`HOKI_ACOUSTIC_SSH=1` (the build-script default) includes `acoustic-link` independently
of the custom UI and Bluetooth SSH switches. Its recipe fetches the pinned
[acoustic-ssh source](https://github.com/0x53A/acoustic-ssh) and builds the Rust
binary with BitBake. No local source checkout or prebuilt payload is needed:

```sh
bash meta-nereid/tools/build-hoki.sh
```

Set `HOKI_ACOUSTIC_SSH=0` to omit it; set all three `HOKI_*` switches to zero
for a stock-layer build. BitBake also builds pinned libquiet/liquid-dsp sources,
with Jansson and PulseAudio's pacat/parec runtime dependencies.
`/usr/bin/acoustic-link` supports both FSK and OFDM plus parallel SSH streams.

Both `acoustic-link.service` (responder) and `acoustic-link-client.service`
(initiator) are **ceres user services**, with explicit disable presets and no
boot enablement. After flashing and when audible testing is intended, start the
responder manually via Wi-Fi SSH or ADB:

```sh
systemctl --user -M ceres@ start acoustic-link.service
systemctl --user -M ceres@ stop acoustic-link.service
```

Starting the responder opens microphone capture; it transmits only after a valid
handshake. Stop ends its capture/playback children. Settings shows an Acoustic
SSH row only when the user service exists. Its off / starting / on / stopping
states follow user systemd; enabling starts it and persists across boots, while
disabling stops it and removes boot enablement. Failed operations are reported.
Physical testing is still pending. The existing audio codec/routing repair is
tracked in [audio status](../knowledge/audio-status.md); this packaging work
neither flashes nor starts audio.

## IIO diagnostic tools

The custom image (`HOKI_CUSTOM_UI=1`) includes `iio-tools`: `lsiio`,
`iio_generic_buffer`, and `iio_event_monitor`, installed in `/usr/bin`. BitBake
fetches the pinned upstream Hoki kernel source and compiles only these userspace
tools. No local kernel checkout, prebuilt payload or manual ELF patching is needed.
Build independently with `bitbake iio-tools`. Nothing starts automatically.

These diagnose IIO devices; they are not required by the sensorfw recorder.
The current Hoki ADC devices expose direct sysfs readings, without buffer/event
interfaces for the capture/monitor tools.
