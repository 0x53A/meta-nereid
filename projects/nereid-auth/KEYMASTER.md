# Keymaster-backed container storage

Author: Lukas Rieger <code@lukasrieger.com>

The maintained native helper now implements a bounded AES-256-GCM wrapping key
and a random 32-byte container passphrase. It uses the resident Qualcomm TA and
kernel QSEE/ION interfaces, without linking Android HAL libraries. This is
source-integrated and locally tested. On 2026-10-03, an initial disposable test
stopped at status `-21`; after correcting parameter offsets, an explicitly
authorized continuation passed wrapping, unwrapping, secret comparison and
temporary-user clearing. Storage was enabled in the 2026-10-04 continuation;
normal PIN unlock created/mounted the 256 MiB container, and normal lock
unmounted it and closed the mapping. A subsequent PIN unlock reopened the same
filesystem and recovered a test file, then removed it. On 2026-10-05, post-reboot
inspection confirmed persistent state, unchanged enrollment/wrapped key and
successful authenticated reopening of the existing container. See the
[reboot health record](../../../_Tasks/20261005_Storage_Reboot_Health/summary.md).
See the [storage continuation](../../../_Tasks/20261004_Secure_Storage_Continuation/summary.md) and the
[successful continuation](../../../_Tasks/20261003_Keymaster_Continuation/summary.md).

## Authentication and key lifetime

The wrapping key permits only AES-256, ENCRYPT/DECRYPT, GCM, no padding and a
128-bit authentication tag. Its hardware-enforced characteristics must contain
exactly the enrolled password SID and PASSWORD authentication. `AUTH_TIMEOUT`,
`NO_AUTH_REQUIRED` and `CALLER_NONCE` are excluded.

Initial setup verifies the PIN once to obtain the SID from a Gatekeeper HAT;
it does not interpret the opaque password handle. It generates the wrapping key,
starts encryption, then verifies the same PIN with the Keymaster operation
handle as the challenge. The authenticated finish wraps a freshly random
32-byte secret. Thus initial wrapping performs two Gatekeeper verifications.
Ordinary unlock obtains an operation handle from decrypt-begin, verifies the PIN
once against that handle, and finishes using the returned HAT. The TEE verifies
the HAT MAC and GCM tag; only a successful final response releases the secret.

The HAT remains inside the native process. Its length, version, challenge, SID
and PASSWORD type are checked before use. No timed authorization, host-side PIN
comparison, unauthenticated encryption fallback or PIN-derived volume key is
used. Failed operations do not retry. A known outstanding operation gets one
abort on ordinary verification failure; broken transport stops further commands.
The helper and its supervisor must both succeed before the service accepts output.

The service keeps transient material in its locked process memory and zeroizing
buffers. It passes the random secret to cryptsetup through a private stdin pipe,
never through D-Bus, argv, environment, logging or a plaintext disk file. This
protects the owned buffers; it cannot eliminate every transient library/kernel
copy. The resulting dm-crypt mapping holds the filesystem key until close.

## Persistent files and opt-in

All files below live in the root-owned mode-0700 `/var/lib/nereid-auth`, bound to
persistent userdata by the matching initramfs. Files are mode 0600. These are
ordinary files on userdata, not a separately flashed partition.

| File | Purpose |
|---|---|
| `credential` | Existing Gatekeeper UID and opaque enrollment handle |
| `keymaster.conf` | Three authoritative vendor version values, one decimal number per line |
| `secure-storage.conf` | Explicit storage opt-in and image size in bytes |
| `volume-key` | Opaque Keymaster blob, SID, generated nonce and wrapped secret |
| `secure.luks` | LUKS2 container file |
| `secure.luks.state` | Durable pending/ready filesystem-provisioning state |
| `wrapping.pending` | Durable interrupted wrapping/setup guard |

With no storage configuration and no storage artifacts, the service retains its
screen-only behavior. With storage enabled, it never silently falls back to that
behavior. Enrollment is explicitly initiated from Settings and leaves the current session
unlocked. The first successful PIN unlock after enrollment provisions storage;
subsequent submissions unwrap its existing secret. Screen unlock is reported only
after the existing/new filesystem has mounted at `/mnt/secure`.

The default suggested size is 268435456 bytes (256 MiB); supported sizes are
64 MiB–4 GiB, multiples of 4096. Provisioning exclusively creates a new file and
never formats an existing image. It persists the wrapped key before formatting.
Uncertain/interrupted setup keeps a recovery marker and blocks automatic retries.
Do not delete markers or regenerate keys as a recovery shortcut: that can strand
an existing container. Preserve the complete private state and container first.

`keymaster.conf` contains, in order: OS_VERSION (`MMmmss`), OS_PATCHLEVEL
(`YYYYMM`), VENDOR_PATCHLEVEL (`YYYYMMDD`), with a final newline. The retained
reference file `deploy/keymaster.conf.hoki-reference` contains:

```text
90000
202112
20211205
```

These values came from a read-only check on 2026-10-01: `/system/build.prop`
reported Android 9 and patch 2021-12-01; `/vendor/build.prop` reported vendor patch
2021-12-05. They describe the current retained vendor environment, not Nereid's
release date and not an independent attestation of boot configuration. Recheck
before deployment if the firmware/vendor image changes. The package installs a
reference copy under `/usr/share/nereid-auth/`; it does not enable storage or
populate private state automatically. There are no zero-version defaults,
automatic key upgrades, global resets or bootloader/root-of-trust provisioning.

The existing initialization now includes the HAL's separate CONFIGURE command
before HMAC sharing for storage operations. It submits the same explicit values
on each isolated helper invocation and stops on error. Same-value configuration
was accepted during both wrap and unwrap in the disposable hardware test;
failures are never automatically retried.

## Filesystem and lock lifecycle

The container uses LUKS2, AES-XTS with a 512-bit combined XTS key, 512-byte sectors,
and PBKDF2 with 100000 iterations. The passphrase is already 256 bits of random
entropy; PBKDF2 avoids the default memory-heavy Argon2 setup on the watch.
`--key-file=- --keyfile-size=32` supplies all binary bytes, including newlines.
Cryptsetup's extra kernel keyring cache is disabled.

The ext4 filesystem disables `orphan_file` and `metadata_csum_seed` for the older
watch kernel. The mount uses `nosuid,nodev` and a root-only mount root. This
implementation does not migrate SSH, Tailscale, Wi-Fi or other application state.
Services using those files will need ordering and shutdown hooks before migration.

`Lock()` first locks the screen, invalidates pending PIN envelopes and blocks new
submissions. It waits for any in-flight authentication, then ordinarily unmounts
and closes the mapping. A stale unlock completion cannot unlock the screen again.
A busy/failed unmount is reported as failure and latches authentication unavailable;
it does **not** mean the filesystem key was evicted. No force/lazy unmount is used.
SIGTERM releases the bus name and performs the same storage cleanup. SIGKILL,
crashes or power failures cannot guarantee cleanup; startup refuses stale mappings
rather than pretending they are secure. Rootfs, recovery access and diagnostics
outside the container stay unencrypted.

## Native interface and evidence

Gatekeeper-only requests retain NGK1/NGR1. Storage uses NGK2 with the same
20-byte header: magic, operation u32LE (3 wrap, 4 unwrap), UID u32LE, handle/PIN
lengths u16LE, wrapped-record length u32LE. Payload is handle, PIN, record.
Wrap requires an empty record; unwrap requires the exact existing NKW1 record.
NGR2 has a 16-byte header: magic, operation, status, payload length. Success returns
32 secret bytes followed by the record for wrap, and exactly 32 secret bytes for
unwrap. Status 1 carries no payload and means generic Gatekeeper authentication
rejection, not proof of a wrong PIN. Indeterminate/backend failures have nonzero
process status and release nothing to the service.

NKW1 disk record: magic4, blob length u32LE, SID u64LE, nonce12, ciphertext/tag48,
then the opaque blob (1–4096 bytes). The returned blob's observed vendor prefix
and hardware policy are checked. On unwrap, GetCharacteristics also requires the
TEE to accept the blob before begin. These match the captured vendor HAL's local
characteristics parser; they are not independent hardware attestation.

Implemented normal TEE commands: CONFIGURE 0x116, GENERATE 0x108,
GET_CHARACTERISTICS 0x109, BEGIN 0x10f, FINISH 0x112, ABORT 0x113. Only fixed
32-byte plaintext / 48-byte ciphertext flows exist; there is no arbitrary command,
key import/export, TA loading or update interface. The version gate remains
API 4.0 / TA 4.162. Response sentinel checks are native client safeguards,
not fields in the vendor request ABI.

Authoritative contracts: [Keymaster 4 operations](https://android.googlesource.com/platform/hardware/interfaces/+/master/keymaster/4.0/IKeymasterDevice.hal),
[Keymaster tags and HAT](https://android.googlesource.com/platform/hardware/interfaces/+/master/keymaster/4.0/types.hal),
[cryptsetup format](https://gitlab.com/cryptsetup/cryptsetup/-/blob/main/man/cryptsetup-luksFormat.8.adoc),
[cryptsetup open](https://gitlab.com/cryptsetup/cryptsetup/-/blob/main/man/cryptsetup-open.8.adoc).
Offline evidence and local validation are recorded in the repository's
`_Tasks/20261001_Keymaster_Integration/` report and delegated contract/wire reviews.

## Validation and remaining hardware gate

The first 2026-10-03 test stopped after the wrapping failure and retained its
temporary enrollment. Following the reviewed fix, a separately authorized
continuation used that same enrollment without enrolling again. Wrap and unwrap
both succeeded, their 32-byte secrets matched in constant time, and authenticated
temporary-user clearing succeeded. The runner then removed its private test
state; a nonsecret attempt marker remains to block accidental reruns. Private
before/after backups are retained on the PC. The real enrollment file remained
byte-identical, auth/compositor stayed active, and QSEE was unowned after normal
supervisor cleanup. Storage configuration and the container were not created.
The existing kernel has the required features; the missing formatter has been
installed from the cached OE e2fsprogs-mke2fs package.

`-21` denotes `INVALID_INPUT_LENGTH` in the Keymaster 4 error enumeration. The
old metadata does not distinguish GENERATE from BEGIN. Offline comparison on
2026-10-03 confirmed a native encoder bug: BYTES parameter offsets must be
relative to the parameter array, not the command. The maintained encoder now
matches that contract for nonce and HAT parameters; scalar-only CONFIGURE and
GENERATE are unchanged. A literal vendor-layout fixture failed before the fix
and passes afterward. The helper also records only the first failed command ID
and status, so later cleanup cannot hide which operation failed. No PIN, token,
nonce, key blob or secret bytes are added to diagnostics.

This correction is locally tested, ARM-built and deployed; the disposable
hardware wrap/unwrap cycle passed with it. This supports the offset bug as the
cause of the old failure, though the original log alone cannot prove which
command returned `-21`.
See the [encoding investigation](../../../_Tasks/20261003_Keymaster_Request_Encoding/summary.md).
The temporary UID has been cleared; neither test runner should be rerun.

Run `bash test-native.sh` and, from this project directory,
`nix-shell --arg nativeOnly true --run 'cargo test -p nereid-auth --locked'`.
Tests use literal synthetic wire fixtures, mock callbacks, private D-Bus and fake
filesystem-command runners; they do not talk to the watch or run host cryptsetup.

The disposable wrap/unwrap gate is complete for the recorded binaries and vendor
configuration. Container creation/mount and lock/close subsequently passed;
reopening also passed with unchanged wrapped key and a surviving test file.
Post-reboot persistence and authenticated reopening also passed on 2026-10-05.
Stop at the first unexpected result. Preserve existing private state;
never validate by migrating the only copy of real credentials. Kernel/runtime
recipe inputs are provided, but a complete image build and deployment are separate.

A future provisioning inbox may publish a public encryption key on userdata while
keeping its private key inside this container. Hosts could queue encrypted Wi-Fi
credentials while locked. Decryption/import would occur only after unlock, with
sender authentication/approval and replay handling added separately. This proposal
is recorded; no inbox keypair or ingestion service is created here.

## Agreed storage split (2026-10-05)

The user approved two independent sparse file-backed filesystems:

| Store | Container capacity | Key authorization | Availability |
|---|---|---|---|
| Device storage | 64 MiB (67108864 bytes) | Separate device-bound Keymaster key, no PIN requirement | At boot, including before first PIN unlock |
| User storage | 256 MiB (268435456 bytes) | Existing PIN-bound Keymaster key | Mounted on PIN unlock, closed on lock |

The user store is the existing validated container. The separate device store
is implemented in [DEVICE-STORAGE.md](DEVICE-STORAGE.md); its no-PIN key
wrap/unwrap roundtrip and initial container creation/mount passed on hardware
on 2026-10-05. Wi-Fi credentials are an intended consumer so networking can
work before unlock without persistent plaintext credential files. No credential
migration or change to the existing PIN-bound key is implied by this record.

Both files should allocate space sparsely. Automatic reclamation of deleted
blocks (filesystem trim through dm-crypt and loop hole punching) is desired but
its policy and implementation are explicitly deferred. Candidate conditions
include sustained charging, a maintenance time window, time since last successful
trim and write activity since that trim. No thresholds or schedule are selected.
Validate discard end-to-end before enabling it; trimming reclaims physical space
without reducing the configured logical capacity. Sparse capacity is not a
reservation of free space on userdata.
