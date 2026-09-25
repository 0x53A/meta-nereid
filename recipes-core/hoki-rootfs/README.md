# File-backed rootfs (initial development implementation)

The recovery kernel uses an initramfs hook to select a rootfs file on the
existing userdata filesystem. No repartitioning or kexec. Without
`.hoki/selection`, the original direct-root and asteroidos.ext4 paths remain.

```
userdata/.hoki/
  selection                 # confirmed trial; '-' means no trial
  versions/VERSION/
    manifest.json
    recovery.img            # matched Android boot-format image
    recovery.sha256
    recovery.size
    rootfs.ext4             # verified, read-only loop mount
    upper/ work/            # writable OverlayFS state unique to this version
  incoming/VERSION/         # incomplete uploads never selected
  state/
    home/ bluetooth/ connman/ tailscale/
    identity/               # SSH host identity, timezone, machine-id
```

The outer userdata is mounted at /userdata in the managed system. Shared state
is bind-mounted before systemd starts. SSH configuration stays versioned; only
the provisioned ECDSA host key is shared. Tailscale starts when shared identity
exists. Initial identities can come from a freshly provisioned seed. Preserve
existing application data and captures before migration; shared-state seeding
does not migrate all old home directories. Do not commit shared state or
provisioning credentials.

Generic rootfs builds can be used after this one-time provisioning. A rootfs
update needs no personalization. Use the existing personalizer to prepare the
initial seed of Wi-Fi credentials, SSH authorized/host keys and Tailscale state;
initialize from that seed mounted read-only. `hoki-rootfs initialize --seed DIR`
creates the store. `--store` permits preparation at the legacy userdata path
before /userdata exists. Initial selection is `legacy -`; installation of a
managed-capable recovery and first trial is a separate migration operation.
A fresh empty userdata filesystem has no legacy fallback: do not use this
initialization route without preparing a confirmed managed version or another
recovery plan. No automated destructive migration is supplied yet.

For a fresh installation with disposable old app data, the workstation can
instead create a complete 4 GiB userdata filesystem containing the first
confirmed version and shared state:

```
python3 tools/provision-rootfs-store.py /path/VERSION provisioning-seed.ext4 userdata.ext4
```

The seed is a freshly personalized copy used only to extract initial credentials
and identities. The nested rootfs comes from the generic bundle. The output
contains secrets, has mode 0600, and must remain private. It preserves the seed's
UIDs/modes and uses an explicit ext4 feature set compatible with Linux 4.14.
It refuses existing output paths and does not touch any device. Flashing this
filesystem replaces userdata contents, not the partition table. Install the
bundle's current `asteroid-hoki-boot.img` recovery artifact alongside it; the
historical `zImage-dtb-hoki.fastboot` alias may point to an old image. First-install
boot validation and recovery testing are still required.

Build a bundle on the workstation:

```
python3 tools/make-rootfs-bundle.py VERSION ROOTFS.ext4 RECOVERY.fastboot /path/VERSION
python3 tools/update-rootfs.py /path/VERSION --host root@WATCH --reboot
```

The upload helper requires an already booted managed system. It uploads to
incoming, checks hashes and the ext4 filesystem, publishes by rename without
another full copy, then selects a trial. It refuses to activate when the actual
recovery partition does not match the bundled recovery image. Kernel updates
remain a separate, deliberate recovery-write operation; this initial helper
never flashes recovery. Normal shutdown/reboot is used, not immediate reboot.

Before mounting a trial, initramfs durably consumes the trial selection. On a
subsequent reboot it uses the confirmed version. A failed mount can fall back
immediately; a partially mounted root stops in the existing ADB recovery path.
The upload helper confirms only after a different boot ID, the expected version,
SSH reachability and active SSH/ConnMan/power/radio/compositor services. This is
basic startup validation, not proof of working audio, sensors, or long-term health.
Without --reboot, confirm explicitly after validation with `hoki-rootfs confirm`.

No watchdog-based automatic hang recovery has been validated. A hung boot may
require a manual reset. No automatic deletion is performed; keep the previous
image while validating. Kernel replacement can make a previous rootfs incompatible;
its presence alone cannot recover from a broken recovery image. Exact recovery
hash matching is deliberately conservative, including initramfs changes.

Mutable state and data migrations are not rolled back. System edits in one
version's overlay do not carry to the next. Move intentional persistent app data
into the explicit state scheme. Diagnostics written elsewhere remain per-version.
Reserve at least 128 MiB after staging for state, metadata and overlays; actual
required space depends on ongoing data growth. Uploads are trusted through SSH;
manifest hashes detect corruption but are not an independent signature scheme.

Run examples from the layer root. Tests: `python3 -m unittest discover -s tests -v`.
An actual watch boot and its USB recovery path remain required before relying on
this updater. Boot support is packaged in the custom layer; it is not retrofitted
by merely installing the userspace command on an old recovery image.

## Kernel updates

See [the public deployment tools](../../../tools/README.md#kernel-only-replacement)
for the guarded kernel-only recovery/metadata replacement. It retains the current
rootfs and overlay and requires unchanged ramdisk/boot parameters. Rootfs-only
updates require exact recovery compatibility; automatic kernel rollback and
power-loss recovery are not supplied.
