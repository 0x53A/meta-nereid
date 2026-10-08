# Nereid authentication

Author: Lukas Rieger <code@lukasrieger.com>.

One privileged service for interchangeable `lock-screen` renderers. The renderer
collects a PIN; the service owns authentication; the compositor owns input and
screen-lock enforcement. Optional storage integration wraps a random container
secret with a PIN-authorized Keymaster AES-GCM key and mounts a file-backed LUKS2
container at `/mnt/secure` before reporting unlock. A disposable PIN-authorized
Keymaster wrap/unwrap cycle passed on hardware on 2026-10-03. Storage was enabled
in the 2026-10-04 continuation: normal PIN unlock created/mounted the 256 MiB
container, normal lock unmounted it and closed the mapping, and a subsequent
PIN unlock reopened the same filesystem with its test file intact. The test file
was removed. A 2026-10-05 post-reboot check confirmed persistent state, unchanged
enrollment/wrapped key, successful authenticated unwrap and the mounted container.
See [Keymaster integration and activation prerequisites](KEYMASTER.md). There is
no PIN-derived disk key or unencrypted key fallback.

Separate [device-bound storage](DEVICE-STORAGE.md) provides a 64 MiB sparse
container without PIN authorization for early-boot use. It has an explicit
provision command and an open-only systemd service. ConnMan integration and
automatic trimming are deferred.

## Encrypted system-bus interface

Bus name/interface `io.Nereid.Auth1`, object `/io/Nereid/Auth1`:

| Member | Result / behavior |
|---|---|
| `GetState()` | `(enrolled: b, locked: b, busy: b)`; errors when backend state is faulted |
| `BeginAttempt()` | `(request_id: ay, public_key: ay)`; both 32 bytes |
| `SubmitPin(request_id: ay, sealed: ay)` | Verify an existing PIN; `(outcome: s, retry_after_ms: u)` |
| `SubmitEnrollment(request_id: ay, sealed: ay)` | Explicit first enrollment from Settings; same result shape |
| `BeginManagement()` | Fresh request ID/key for an enrolled, unlocked session |
| `ManagePin(request_id: ay, sealed: ay)` | Authenticated change or clear; same result shape |
| `Lock()` | Locks enrolled devices and invalidates pending envelopes; healthy unenrolled devices stay unlocked |
| Properties | Read-only `Enrolled`, `Locked`, `Busy`; changes are signalled |

The `nereid_auth::Client` Rust API handles encryption. Renderers should use that
API instead of constructing envelopes. `BeginAttempt` creates one fresh libsodium
sealed-box recipient keypair for one request. The private key occupies guarded,
locked memory and is never persisted. One outstanding attempt is allowed; the
same caller can replace its own attempt. It expires after 30 seconds (the daemon
reaps once per second). A matching submission consumes the key **before** checking
its ciphertext, including malformed submissions and authentication failures.
Decryption completes and the key is wiped/freed before hardware authentication.

Each request is bound to its D-Bus unique sender. The client pins both calls to
the same service owner, so a restart cannot transparently redirect a submission.
The plaintext has fixed length 64: version byte 1, request ID (32), PIN length
(1), 4–12 ASCII digits, then zero padding. Sealed ciphertext is 112 bytes. No PIN
appears in bus arguments, signals, logs, environment variables or command lines.
Local tests inspect actual bus-monitor messages using dummy credentials.

Management uses version 2 of the fixed 64-byte envelope: request ID at bytes
1–32, action at 33 (1 change, 2 clear), current/new lengths at 34/35, then both
PINs and zero padding. Clear has no new PIN. Keys are bound to the management
purpose as well as their sender; ordinary verification keys cannot change a PIN.
The client exposes `begin_management`, `change_pin`, and `clear_pin`.

The system-bus policy permits only root to own the service name and root/ceres to
call it. In the current threat model apps sharing ceres are trusted: this policy
does not distinguish the renderer from other ceres processes. Encryption protects
passive bus captures, not a compromised service, malicious root, or a replaced OS.
It is not a secure-boot substitute. Callers still briefly hold plaintext input.

## Backend and state

The native backend implements ordinary Gatekeeper enroll/verify against the
already-resident, version-checked vendor TA and uses the reviewed RPMB listener.
It also supports authenticated PIN change and per-user deletion. There is no app loading, firmware update, global reset, or arbitrary
command API. Storage operations use the additional bounded Keymaster flow in
[KEYMASTER.md](KEYMASTER.md). The daemon persists only an opaque credential and random UID in
`/var/lib/nereid-auth`, directory root 0700, files root 0600. It does not consume
historical disposable test state.

PIN setup is optional and belongs to Settings. A healthy backend with no credential
starts unlocked, and `Lock()` leaves it unlocked. The ordinary lockscreen does not
enroll, and `SubmitPin` never creates a credential. Settings explicitly submits
`SubmitEnrollment` after PIN confirmation. Successful enrollment leaves the current
session unlocked; the next lock or service restart requires the new PIN. With
storage enabled, the first subsequent PIN unlock provisions the container.
Unreadable credentials, interrupted enrollment and service failures are not treated
as no PIN: they fail closed. A durable `enrollment.pending` file precedes the hardware enrollment and
is removed only after the credential has been fsynced. Interrupted/failed
enrollment requires explicit recovery; deleting this marker to retry is unsafe.
Infrastructure errors latch the daemon unavailable until inspected/restarted.
No operation is automatically retried. Generic status -30 is surfaced as a
rejection, without claiming a specific cause; other unclassified statuses fail
closed. The native backend owns bounded listener-first cancellation.

Settings **PIN Management** offers setup when unenrolled and Change PIN/Clear PIN
when enrolled. Change requires the current PIN and two matching new entries.
The backend verifies the current credential, changes it using the old handle,
and verifies the new handle has the same secure user ID before saving it.
Clear requires the current PIN plus confirmation that screen locking will be
disabled. It verifies once, deletes only the enrolled Gatekeeper user, and then
removes the local credential. It is not a TEE reset. Clear refuses existing
encrypted-storage state (`storage-protected`); removing that protection requires
a separate storage migration or explicit destruction workflow.

A durable `management.pending` marker precedes either hardware mutation.
Successful changes use `credential.next`, atomic rename, and file/directory
fsync. Unknown outcomes retain recovery state and block further authentication;
never delete markers merely to retry. Only a well-formed, pre-mutation generic
verification rejection clears the marker without changing the credential.

The daemon requires root, disables dumps, locks its memory, and fails startup if
these protections cannot be established. The systemd unit grants memlock and does
not automatically restart after failure. Do not enable core dumps for debugging
credential-bearing processes. Renderer-owned PIN buffers must also be wiped and
protected; the supplied renderer does so. No claim is made that every transient
compiler/runtime copy can be eliminated.

## Building and validation

From this directory:

```sh
nix-shell --arg nativeOnly true --run 'cargo test -p nereid-auth --locked'
nix-shell --run 'cargo build -p nereid-auth --locked --release --target armv7-unknown-linux-gnueabihf'
```

The host integration tests start disposable private D-Bus daemons and use a dummy
backend, never the system bus or watch. Production uses system D-Bus and the real
native helper; there is no runtime option that selects the dummy backend.
`bash test-native.sh` runs native wire/lifecycle fixtures with ASan/UBSan and
supervisor tests, without a watch or real block mappings. Container tests use a
fake command runner; they do not format or mount host storage.

The auth service, Settings enrollment and compositor lock role were deployed on
2026-10-01. Live checks confirmed an empty, unlocked state, ceres bus access and
no lockscreen process on no-PIN startup. No PIN was enrolled by deployment, and
encrypted storage was initially disabled. On the current older initramfs, an fstab bind
mount persists `/var/lib/nereid-auth` at `/userdata/.hoki/state/nereid-auth`; the
service waits for that mount. The user subsequently enrolled and confirmed a
successful unlock after reboot. Future
matching boot images use the persistent-state manifest described below.

The PIN Management service/native/UI update was subsequently deployed on the
same date. Local wire, service and UI tests passed; installed services are healthy
and the existing credential is unchanged. No hardware PIN change or clear was
performed during this deployment; those user actions remain to be exercised in
the new UI. See the workspace `_Tasks/20261001_PIN_Management` record.

Install the unit and bus policy together. Keep recovery SSH accessible. The image must supply libsodium and
the native helper set, not just `nereid-authd`.

The rootfs persistent-state manifest now includes `var/lib/nereid-auth`. That
manifest is also embedded in the boot/initramfs image: a rootfs-only deployment
with an older initramfs does not establish persistence for this new directory.
Validate the matching boot image/state bind mount before relying on update
persistence, and keep a private backup before any image replacement.
