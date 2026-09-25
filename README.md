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

Check this out as `meta-nereid/` beside the applications in 0x53A/asteroid-watch.
Acoustic SSH is fetched from 0x53A/acoustic-ssh and built by its Cargo recipe.
The remaining runtime build/fingerprint helpers use sibling application
sources and Nix-built bundles. GPS-recorder and NFC recipes also use sibling
source directories. Generated runtime archives are ignored and must be rebuilt
after source or build-path changes; see [APPS.md](APPS.md). A standalone clone
of this layer cannot build the complete image.

Host image building, SSH upload, provisioning, and recovery replacement remain
in the root repository's `tools/`. The on-watch version manager, persistent-state
policy and matching initramfs hook stay together here. See the
[rootfs guide](recipes-core/hoki-rootfs/README.md).

The image wrapper enables meta-hoki-ex and meta-nereid under the same custom
UI/transport switches that enabled the former meta-hoki-local layer. Existing
HOKI_* controls and package names are preserved. Generic fixes carried for this
stack have not been relocated to upstream layers as part of this extraction.
