# Custom Hoki apps

`HOKI_CUSTOM_UI = "1"` selects `hoki-ui` and `packagegroup-hoki-apps`.
The existing Nix cross-build produces `hoki-runtime.tar.gz`; BitBake's
`hoki-ui` recipe splits the app files into individual packages. This remains
a prebuilt-payload workflow, not a Cargo build performed inside BitBake.

The [intended audiobook migration](projects/hoki-audiobook/BUILD-DEPS.md#intended-production-build-not-yet-implemented)
is a source-building BitBake recipe using real sysroot libraries. It is deferred;
the current runtime-bundle workflow remains in use.

All custom externally built payload packaging belongs in `meta-nereid`,
including UI, BLE SSH and health-recorder. Acoustic SSH and hardware GPS helpers
are already built from source by their respective layer recipes.
Keep the upstream meta layers usable without these bundles or root-repository
build outputs. Source-building recipe migration and packaging cleanup are
deferred. Both stock and custom configurations should remain supported; a
custom-image build alone does not validate the stock configuration.

Build from the repository root:

```sh
bash meta-nereid/build-runtime.sh
python3 meta-nereid/check-runtime.py meta-nereid/recipes-hoki/hoki-ui/files/hoki-runtime.tar.gz
bash meta-nereid/tools/build-hoki.sh
```

The build uses each project's own `shell.nix`, patches the ARM loader/RPATH,
and validates binaries, launcher entries and checksums before publishing the
archive. Fingerprinting covers the project manifest, sources, wrappers,
audiobook cross-link inputs and WASM guest sources. The WASM guest is rebuilt
before embedding it. The full image build rejects a stale archive.

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
| hoki-wasm-host + hoki-wasm-guest | `hoki-wasm-host` package with embedded guest |
| asteroid-compass | Existing patched upstream recipe retained |
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
package group, including their runtime dependencies. `check-runtime.py`
checks each application has a desktop entry and matching wrapper. Packaging
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
