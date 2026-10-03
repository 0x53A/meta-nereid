# Build and deploy Hoki images

These tools build images and manage deployments over SSH. Run the examples from
a workspace containing `asteroid/` (the AsteroidOS assembler), `meta-asteroid/`,
`meta-smartwatch/`, `meta-hoki-ex/`, `meta-nereid-sdk/`, and this `meta-nereid/` checkout. Use mutually
compatible, pinned revisions; layer dependencies target Whinlatter.

## Build

The workstation needs Bash, Python 3.11 or newer, rsync and OpenSSH. The remote Linux builder needs rsync, Bash and rootless Podman;
its container is built from the assembler's Dockerfile. Both machines need access
to source repositories and sufficient space for build caches and images. The
recorder's SSC helper uses the pinned `android-ndk-native` BitBake recipe.

Configure the SSH destination and a dedicated remote staging directory:

```sh
export NEREID_BUILD_HOST=builder
export NEREID_BUILD_DIR=/srv/nereid-build
export NEREID_BUILD_THREADS=6
export NEREID_MAKE_JOBS=4
bash meta-nereid/tools/build-hoki.sh
```

For a local build, set `NEREID_BUILD_HOST=local` and choose a dedicated build
directory outside the source workspace. Local builds support Docker or Podman:

```sh
export NEREID_BUILD_HOST=local
export NEREID_CONTAINER_RUNTIME=docker
export NEREID_BUILD_DIR=/mnt/T5/workspaces/hoki/build/nereid
export NEREID_IMAGE_DIR=/mnt/T5/workspaces/hoki/build/images
export NEREID_BUILD_THREADS=2
export NEREID_MAKE_JOBS=4
bash meta-nereid/tools/build-hoki.sh
```

In the asteroid-watch workspace, `nix-shell` supplies the host tools through the
root `shell.nix`; a running Docker daemon or rootless Podman is also required.
`NEREID_CONTAINER_RUNTIME` defaults to `podman` for existing remote builds.
Docker runs compilation as the invoking user's UID/GID so output stays writable.
Its build container disables Docker's default seccomp filter to allow BitBake's
unprivileged user/network namespaces; it does not use privileged mode.

Keep machine-specific exports in an ignored local file. The wrapper defaults to
the workspace containing this layer; override with `NEREID_WORKSPACE`. Outputs
go to the workspace's `images/`, or `NEREID_IMAGE_DIR`. Keep images, generated
runtime archives, build logs and caches ignored. Keep retained backups and
credentials in a separate ignored directory that you transfer between machines.

The wrapper checks generated Cargo dependency metadata, mirrors the source layers (including
the SDK-only `meta-nereid-sdk` layer) into
the dedicated remote directory, prepares the assembler, builds `asteroid-image`
and two standard SDKs, downloads their artifacts and checks the boot image's
initramfs. The full image SDK is distributed as `nereid-full-sdk` and includes
Qt. `nereid-small-sdk` is a smaller SDK
for external Rust/Slint apps: it supplies the target C toolchain,
Wayland, XKB and font development libraries, while the developer/CI
host supplies rustc and Cargo. It does not contain Qt development packages.
SDK installers, host/target package manifests, BitBake metadata and SHA-256
files are downloaded to `images/sdk/<rootfs-image-name>/` (or under
`NEREID_SDK_DIR` when set). The two installers are built from the same machine,
distro and layer revisions as that rootfs. Retain their manifests with the
installers when copying them to other machines or CI.
`NEREID_BUILD_THREADS` limits simultaneous BitBake tasks and
`NEREID_MAKE_JOBS` limits compilation jobs and XZ/Zstandard compression threads
within each task. On a 32 GB builder,
start with three BitBake tasks and four compile jobs to leave memory headroom.
Mirroring replaces staging contents; use a dedicated directory without other work.
It does not deploy. `HOKI_CUSTOM_UI`, `HOKI_BLE_SSH`, and `HOKI_ACOUSTIC_SSH` each
default to `1` and accept `0` to disable the corresponding selection.

UI, BLE SSH, health recording, GPS helpers, acoustic SSH and IIO tools are built
by source recipes. The upload stages only declared project inputs, excluding
local caches and private data links. After Cargo.lock or project inventory
changes, run `python3 meta-nereid/tools/update-runtime-recipes.py` and review its
outputs. See [the build inventory](../APPS.md) for remaining binary inputs.
The target Rust recipes also generate per-artifact Cargo SBOM precursors in their
normal BitBake compile step. The build downloads them to `images/cargo-sbom/<recipe>/`
alongside a `.licenses.json` report for each artifact. Those reports use the
exact crate IDs selected by the compile step and the license declarations from
the fetched Cargo manifests. Review missing declarations and actual license
notices before distributing the binaries; the reports are not a legal conclusion.
This BitBake-only option does not change ordinary desktop Cargo builds.
The first source build also builds WebAssembly support in native LLVM; subsequent
builds can reuse the normal BitBake shared-state cache.

Yocto's full image SPDX is deployed as `images/asteroid-image-hoki.rootfs.spdx.json`
beside the rootfs. `make-rootfs-bundle.py` copies it into the versioned watch
bundle as `sbom.spdx.json`, generates `licenses.tsv` from installed SPDX packages
and present Cargo artifacts, and archives Cargo reports when available. These
files remain beside `rootfs.ext4` under `/userdata/.hoki/versions/VERSION/` on
the watch, readable over root SSH. The Settings Licenses page reads the image's
`/usr/share/common-licenses/license.manifest`; its on-screen list therefore
shows installed Yocto packages, while the sidecars carry crate detail. For
first installations made with `provision-rootfs-store.py`, supply a bundle made
with the same tool so the sidecars are retained with the version.

## Managed rootfs updates over SSH

Read [the rootfs guide](../recipes-core/hoki-rootfs/README.md) first. Once a watch
has a managed rootfs store and provisioned shared identities, updates use generic
images; no per-update personalization is needed.

```sh
python3 meta-nereid/tools/make-rootfs-bundle.py VERSION \
  images/asteroid-image-hoki.rootfs.ext4 \
  images/asteroid-hoki-boot.img bundles/VERSION
python3 meta-nereid/tools/update-rootfs.py bundles/VERSION \
  --host root@WATCH --host-key-alias WATCH --reboot
```

Choose an unused version and an ignored bundle output directory. Before any
reboot, preserve relevant system/user journals, sensor captures and metadata via
SSH, verify the copies, and coordinate any ongoing recording. Userdata and shared
identities survive managed updates; version-specific overlay edits do not migrate.

The uploader requires an exact match with the installed recovery image. It never
flashes recovery. It transfers with compression and sparse-file preservation,
verifies the rootfs, selects a trial, then checks SSH and essential services before
confirming when `--reboot` is supplied. Startup
checks do not establish full audio, radio, sensor or display regression coverage.

## Initial image personalization

Full userdata flashing replaces its contents. Preserve existing data first. Run
personalization locally on each newly built, unmounted ext4 image; credentials
must never enter the public layer or remote build. Explicitly select SSIDs and
the existing stable ECDSA host key (with matching `.pub` file):

```sh
python3 meta-nereid/tools/personalize-image.py \
  --wifi-ssid 'YOUR_NETWORK' \
  --ssh-host-key /path/to/private/ssh_host_ecdsa_key \
  --ssh-key ~/.ssh/id_ed25519.pub \
  --tailscale-state /path/to/private/tailscaled.state \
  images/asteroid-image-hoki.rootfs.ext4
```

Repeat `--wifi-ssid` and `--ssh-key` as needed. SSIDs are network names, not
NetworkManager connection profile names. Passwords are read through local
NetworkManager permissions. Open and personal WPA-PSK/SAE profiles are supported;
enterprise/WEP/hotspot and ambiguous profiles are rejected. Exporting an SAE
password does not establish that the watch supports connecting to that network.

Dependencies: Python 3.9+, NetworkManager (`nmcli`), e2fsprogs (`debugfs`, `e2fsck`),
OpenSSH (`ssh-keygen`) and GNU coreutils (`cp`). No mount or sudo is needed.
The tool validates and edits a temporary image, preserves the original, and only
publishes output after checks pass. The default output is a sibling
`.personalized.ext4` with mode `0600`. Never use stale output after a failure.

The default authorized key is the first available of `id_ed25519.pub`,
`id_ecdsa.pub`, or `id_rsa.pub` in `~/.ssh/`; explicit `--ssh-key` overrides this.
Existing authorized-key restrictions are preserved. The host private key must
have no group/world access. The host timezone is copied unless `--timezone` is
specified. `--enable-ble-ssh` enables the packaged Bluetooth SSH service and
Bluetooth in ConnMan; otherwise their existing configuration is preserved.

For a new managed installation, use the personalized image only as the identity
seed for `provision-rootfs-store.py`, as described in the rootfs guide. Flashing
its output destroys existing userdata. Hoki boots the kernel from **recovery**,
not boot. Use `asteroid-hoki-boot.img`; the legacy `zImage-dtb-hoki.fastboot` alias
may be stale. Preserve data before bootloader entry and verify SSH and the ceres
user compositor after boot.

### Tailscale identity across reflashes

`--tailscale-state` restores the normal file backend's identity and enables the
packaged service. Before a full reflash, check the authenticated watch's Tailscale
status and refresh `/var/lib/tailscale/tailscaled.state`. Over a separate direct
SSH connection, coordinate a brief stop/copy/start of `tailscaled` for a consistent
snapshot. Never stop it over the only connection to the watch. Verify the new
copy before atomically replacing the previous backup; retain mode `0600` and
never print the contents. Expired identities need reauthentication and a fresh
snapshot. Do not clone one device identity onto multiple running devices.
Encrypted/TPM and additional Tailnet Lock state need a separate migration.

## Kernel-only replacement

`replace-recovery.py` runs on the watch and requires a managed, confirmed rootfs
with no pending trial. It only accepts a replacement that preserves the installed
ramdisk and boot parameters. It checks current boot/version and hashes, creates
durable backups, verifies the write, and updates matching recovery metadata.
It does not reboot. Ordinary errors attempt verified restoration; power loss
during the physical write still needs external recovery. Rootfs rollback cannot
restore a bad kernel.

1. Finish or coordinate captures, preserve and verify data and journals, then
   read current boot ID, confirmed version, battery, free space and full recovery
   partition hash. Back up the partition and version metadata off-device.
2. Prepare a compatible kernel-only image using the actual installed ramdisk and
   header; validate image identity, kernel configuration and module ABI. The
   helper is not an image packager and rejects a changed ramdisk/header.
3. Upload the helper and image to a private userdata directory. Run it as a
   systemd oneshot that survives SSH loss, without a timeout interrupting writes.
   Required flags: `--image`, `--expected-version`, `--expected-boot-id`,
   `--old-partition-sha256`, and `--image-sha256`. Use freshly observed values.
4. Require successful completion and the durable result file. Independently
   verify partition/metadata hashes, unchanged selection and rootfs identity.
   If restoration failed, repair recovery before rebooting.
5. After a normal reboot, verify a new boot ID, expected version, system and
   ceres user services. `read-kernel-canaries.py` can save JSON snapshots and
   compare same-boot counters; these are diagnostics, not hardware proof.

## Tests

From the layer root:

```sh
python3 -m unittest discover -s tools/tests -v
python3 -m unittest discover -s tests -v
bash -n tools/build-hoki.sh
```

Tests cover host-side image manipulation and transaction failure handling.
Device boot, network association and hardware validation remain separate.

## Compressed managed roots

Hoki image builds export `.ext4` and `.squashfs-lz4` from the same fakeroot tree.
SquashFS uses LZ4 and 128 KiB blocks; ownership, modes, links and xattrs come
from the image build rather than an unprivileged extraction. The kernel enables
SquashFS/LZ4 and retains ext4, loop and OverlayFS. Userdata stays ext4.

`make-rootfs-bundle.py` accepts either extension and emits format 2 for SquashFS
with explicit `rootfs_type` and `rootfs_file`. Ext4 output retains format 1 for
existing watches. The manager and provisioner accept both manifest formats. SquashFS validation streams a pseudo-file representation
to a discarded output: it traverses and decompresses the complete filesystem
without storing a second extracted root. Target `squashfs-tools` is required.
SPDX, package manifest and Cargo reports must belong to the same build.

The first compressed-root deployment is a two-boot migration, not a normal
rootfs-only upload:

1. Preserve current recovery, version metadata, relevant logs and captures.
   Review the new initramfs and check its exact source, boot parameters, kernel
   configuration and module compatibility with the currently confirmed ext4 root.
2. `replace-recovery.py` defaults to kernel-only behavior. For an explicitly
   reviewed initramfs migration, provide both `--old-ramdisk-sha256` and
   `--new-ramdisk-sha256` in addition to the ordinary exact image/partition,
   version and boot-ID guards. This permits only the specified ramdisk change
   and its header size; all other boot parameters must remain identical. The
   same locked backup/readback/metadata transaction and error restoration apply.
3. Reboot and validate the new recovery with the existing confirmed ext4 root.
   Rootfs rollback cannot restore recovery. Keep the full old partition backup
   off-device for external recovery.
4. On that old root, install the new `hoki-rootfs` manager and a compatible
   `unsquashfs` plus its dependencies before staging a compressed bundle. These
   tools are already included in new image builds, but an old root lacks them.
5. Upload the paired SquashFS bundle without `--reboot`, then reboot and validate
   the trial, compressed lower mount, overlay writes, SSH, UI, radios, sensors
   and audio before explicit confirmation. Retain the confirmed ext4 fallback.

Both format-2 filesystem types use the same exact recovery hash requirement and
128 MiB free reserve. Never flash a SquashFS file directly onto userdata: it is
a nested managed root image, not the writable userdata filesystem.
