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
shared support modules. Runtime builders and direct GPS/NFC source recipes use
paths within this layer. Acoustic SSH is fetched from 0x53A/acoustic-ssh and built
by its Cargo recipe; armagnac, bluer and dbus-rs use pinned GitHub Cargo revisions.

The UI, BLE SSH and health-recorder recipes still package Nix-built runtime
bundles. Moving their source here does not convert them to BitBake compilation.
Generated archives remain ignored and must be rebuilt after source changes;
see [APPS.md](APPS.md). The recorder's SSC helper requires an Android NDK, and
some media cross-build inputs are still supplied separately. Those existing
build requirements remain until the source-recipe migration.

Host image building, SSH upload, provisioning and recovery replacement live in
[tools/](tools/README.md), with explicit local configuration for build hosts and
provisioning identities. The on-watch version manager, persistent-state policy
and matching initramfs hook live here too. See the
[rootfs guide](recipes-core/hoki-rootfs/README.md).

The image wrapper enables meta-hoki-ex and meta-nereid under the same custom
UI/transport switches that enabled the former meta-hoki-local layer. Existing
HOKI_* controls and package names are preserved. Generic fixes carried for this
stack have not been relocated to upstream layers as part of this extraction.
