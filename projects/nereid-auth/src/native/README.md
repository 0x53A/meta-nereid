# Native Gatekeeper backend staging

Maintained native backend for the Rust service. It attaches only to resident
`keymaster64` or `keymaster`, gates on API 4.0 / TA 4.162 and uses the captured
version initialization and HMAC sharing. NGK1 supports enrollment/verification;
NGK2 adds narrowly scoped PIN-authorized Keymaster wrapping/unwrapping for the
container. NGK3 adds authenticated PIN change and per-user clear. It never loads
firmware, globally resets the TEE or programs RPMB keys.
The storage extension is locally tested, not yet hardware-validated.

## IPC

The Rust service sends one frame over stdin and closes the pipe:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | ASCII `NGK1` |
| 4 | 4 | Operation, u32 little-endian: 1 enroll, 2 verify |
| 8 | 4 | Service-generated stable Gatekeeper UID, u32 little-endian; zero is rejected |
| 12 | 2 | Opaque prior-handle length, u16 little-endian |
| 14 | 2 | PIN byte length, u16 little-endian |
| 16 | 4 | Reserved, must be zero |
| 20 | variable | Handle bytes followed by PIN bytes |

The helper bounds handles to 1–1024 bytes for verify and PINs to 1–64 bytes;
enroll requires an empty handle. The parent service applies the user-facing
4–12 ASCII-digit PIN rule. The frame must end exactly after the payload.

The reply is `NGR1`, operation u32LE, signed Gatekeeper status i32LE, handle
length u32LE, then an opaque handle only for successful enrollment. Verification
returns a status with no token. TEE status values remain signed and uninterpreted
by the transport; in particular, `-30` is not sufficient by itself to classify
every failure as an incorrect PIN. Helper or listener infrastructure errors
produce a nonzero process exit; a reply may already have been emitted before
a cleanup failure. Callers must require both a valid reply and exit 0, and must
not retry after a
nonzero exit because the secure-world operation may already have run.

`backend.py` is the installed entry point expected at
`/usr/libexec/nereid-auth/backend.py`. Install `backend.py`, `supervisor.py`,
`nereid-gatekeeper-backend`, and `rpmb-listener` together in a root-owned,
non-writable directory. The Rust service must pass a bounded frame through
stdin, close stdin, validate the matching NGR1/NGR2/NGR3 reply, and persist the returned UID and
enrollment handle in its own root-only state. Use atomic replacement plus file
and directory `fsync`; handle files must be root:root mode 0600 and their
directory root:root mode 0700. The backend intentionally stores no identity or
handle itself.

The caller should treat a nonzero process exit as an indeterminate operation,
retain the persistent UID, and block automatic re-enrollment/retry until an
explicit recovery path exists. `backend.py` returns exit 0 only when the native
helper exits successfully and the listener has stopped cleanly. It relays stdin
directly into C and helper stdout directly to the Rust caller, so Python never
copies PINs or handles into its own memory.

The wrapper takes an exclusive `flock` on
`/run/nereid-auth-backend.lock` and requires `fuser /dev/qseecom` to return 1
with empty stdout/stderr before listener registration. The installed service
therefore needs a `fuser` provider (normally `psmisc`), plus Python's
`python3-core`, `python3-threading`, and `python3-fcntl` modules.

The native helper requires `LimitMEMLOCK=infinity` and disables core dumps and
ptrace dumping before reading credentials. Its C process locks memory, validates
the exact request bounds, wipes local/shared credential buffers, and emits no
secret diagnostics. Listener-first cancellation and the shared ten-second
cleanup budget follow the reviewed implementation.

## Build

NGK3 uses the same 20-byte header with operation 5 (change) or 6 (clear),
the existing UID, prior-handle length, current-PIN length, and a new-PIN u32LE
length at offset 16. Payload is handle/current/new, with no trailing bytes.
Both PINs must be 4–12 ASCII digits; clear requires zero new-PIN length.
NGR3 is magic, operation u32LE, status u32LE, payload length u32LE and payload.
Status 0 returns a new handle for change or no payload for clear. Status 1
has no payload and means only the first verification returned generic `-30`
before any mutation. All other errors, including post-change verification
failure, exit nonzero. Current/new verification must report the same nonzero
SID. Clear verifies the current PIN then calls per-user `GK_DELETE_USER` once.
Neither flow retries. These paths have mocked wire/sequence tests; the earlier
disposable-device change/delete experiment is not a live validation of this UI.

Run `bash build-native.sh` from the enclosing `nereid-auth` project directory to
compile both binaries with strict warnings for ARMv7 and apply the watch ELF
interpreter/RPATH settings. Host parser/supervisor tests live beside this file.
No hardware operation is performed by building or running those host tests.

## Keymaster and encrypted container

The maintained helper now retains a successful HAT only long enough to authorize
one Keymaster AES-GCM operation. NGK2/NGR2, the NKW1 private record, version
configuration, key policy, storage lifecycle and remaining hardware validation
are specified in [KEYMASTER.md](../../KEYMASTER.md). NGK1 replies still expose no
authentication token. Storage replies carry a random secret only through the
private root-service pipe, never through D-Bus.
