<p align="center">
  <img src="assets/certified-slop.svg" alt="100% Certified Slop" width="640">
</p>

> [!NOTE]
> This project was largely LLM generated.

# Nereid

Custom watch UI, applications, companion services and image policy on AsteroidOS.
Includes the compositor/app runtime packaging, Qt integration, health recording,
Bluetooth/acoustic SSH, media features, and versioned file-backed rootfs updates.

Requires `core`, `asteroid-layer`, and `hoki-ex` (plus their dependencies),
Whinlatter. Current packages and image overrides target Hoki; this extraction
does not yet make Nereid portable to other watches.

## Build integration

Application and service source lives in [projects/](projects/README.md), including
shared support modules. BitBake compiles the UI/apps, Bluetooth SSH and health
recorder directly, with locked crate downloads and real target sysroot libraries.
The SSC helper uses `android-ndk-native`; the embedded WebAssembly demo and its
Rust standard library are built from source. Acoustic SSH is fetched from its
pinned repository. See [APPS.md](APPS.md) for the build inventory.

Nix shells and runtime bundle scripts remain for standalone development and
legacy direct deployment. They are not inputs to the image build. Tailscale,
Android compatibility/vendor libraries and firmware remain fetched binary
inputs, and the Android NDK is a fetched native toolchain.

Host image building, SSH upload, provisioning and recovery replacement live in
[tools/](tools/README.md), with explicit local configuration for build hosts and
provisioning identities. The on-watch version manager, persistent-state policy
and matching initramfs hook live here too. See the
[rootfs guide](recipes-core/hoki-rootfs/README.md).

## Watch time

Hoki's hardware RTC is read-only. `swclock-offset` restores system time from
the RTC plus its saved offset at boot, without requiring a network connection,
and saves the offset at shutdown. This relies on continuity of the RTC counter;
loss/reset of the counter cannot establish elapsed offline time by itself.

The image enables upstream `systemd-timesyncd` for NTP correction when Internet
access is available. `swclock-offset-sync.path` saves the offset after network
synchronization, so the corrected value does not depend only on clean shutdown.
The meta-asteroid timesyncd exclusion is explicitly disabled for Hoki; appending
`timesyncd` to PACKAGECONFIG alone does not override a `:remove`.

The Android companion can seed time and set the timezone over authenticated
SSH, including its BLE tunnel, through `hoki-companion-agent`. It preserves an
NTP-synchronized clock. Saving the software RTC offset immediately after a phone
correction remains a follow-up; the existing shutdown save still applies.
Phone end-to-end validation is outstanding. The watch has no cellular time
source and may remain without Internet access for extended periods.

The image wrapper enables meta-hoki-ex and meta-nereid under the same custom
UI/transport switches that enabled the former meta-hoki-local layer. Existing
HOKI_* controls and package names are preserved. Generic fixes carried for this
stack have not been relocated to upstream layers as part of this extraction.
