# Device-bound storage

Author: Lukas Rieger <code@lukasrieger.com>.

The separate 64 MiB sparse LUKS2/ext4 container is stored at
`/var/lib/nereid-auth/device/secure.luks`, mapped as `nereid-device`, and mounted
root-only at `/mnt/device`. The parent auth directory already has a persistent
userdata bind mount. The 256 MiB PIN-bound user container is independent.
Container capacities include the LUKS header; the device filesystem has about
48 MiB available before ext4 overhead. Sparse capacity does not reserve space.

The resident Qualcomm TEE Keymaster generates an AES-256-GCM key with
`NO_AUTH_REQUIRED`, without SID or authentication-type tags. The helper checks
the hardware-policy prefix and authenticates the opaque blob in the TEE. It
wraps a random 32-byte container passphrase; only the blob, nonce and ciphertext
are stored. `NGD1`/`NDR1` operations 7/8 and `NDW1` records are separate from
the PIN protocol and `NKW1` records. Neither path accepts the other's policy.
There are no Gatekeeper enrollment/verification/deletion calls on this path.
The existing backend lock serializes use of QSEE and the temporary RPMB listener.

This protects an offline copy of userdata. A privileged running OS can request
device-key unwrap without a PIN. Verified boot enforcement is not established,
and this is a TEE-backed key, not a verified discrete secure element.

## Provisioning and startup

Build using the auth project's normal Rust/native build and ELF-patching steps.
The image recipe installs the binary and unit but does not automatically enable
the device service on an unprovisioned watch. Provisioning is a separate explicit
root operation, with locked memory and core dumps disabled:

```sh
systemd-run --unit=nereid-device-provision --wait \
  --property=LimitMEMLOCK=infinity --property=LimitCORE=0 \
  --property=UMask=0077 --property=Type=oneshot \
  --property=Environment=PATH=/usr/sbin:/usr/bin:/sbin:/bin \
  --property=TimeoutStartSec=180 \
  /usr/libexec/nereid-device-storage provision
```

It exclusively creates a durable `wrapping.pending` guard, saves the wrapped
key, verifies a fresh unwrap matches, then creates the filesystem. Only completed
provisioning clears the guard. A failed or interrupted operation requires
inspection and explicit recovery; never remove a marker just to retry. The
normal `open` command cannot create or replace a key or container.

After validating mount/close on the provisioned device:

```sh
systemctl enable --now nereid-device-storage.service
systemctl stop nereid-device-storage.service
systemctl start nereid-device-storage.service
```

The oneshot unit runs in the real-root systemd transaction after switch-root:
`DefaultDependencies=no`, required mounts for `/usr` and `/var/lib/nereid-auth`,
after udev trigger, before `local-fs.target` and PIN auth. It waits at most 15
seconds for QSEE, ION and RPMB nodes. It does not require D-Bus or networking.
`After=mnt-device.mount` does not pull in a mount job during boot: systemd learns
this mount after the helper creates it. The ordering then ensures the stop
command runs before systemd's unmount job during shutdown. Stop performs ordinary
unmount followed by mapping close; busy filesystems fail visibly, without force.

No service consumes this filesystem yet. ConnMan and its credential files are
unchanged. A future consumer must explicitly require and order itself after
device-storage availability; an ordering edge alone does not propagate failure.
Any credential-directory bind mount must replace the existing plaintext bind,
not merely cover it after the consumer starts. Boot failure currently leaves
this optional test service failed while the rest of the watch can boot.

Automatic vacuum/trim and `allow-discards` are deferred. No maintenance policy
or credential migration is implemented.

## Validation status (2026-10-06)

Real no-PIN wrap/unwrap, provisioning, three mount/read/close cycles and refusal
to reprovision passed on the watch. Host protocol, memory-sanitizer, Rust/D-Bus
tests and a systemd ordering transaction passed. The service is enabled there.

After reconnecting on October6, a new boot confirmed the device mount completed
at9.735744seconds, before local-fs.target at9.736303 and before auth started
at11.579040. The later PIN-authorized user-key unwrap also succeeded. All original
credential and wrapped-key hashes remained unchanged. The persistence test file
survived and was removed. Closed device-image/auth backups and the interrupted
capture preservation are verified. ConnMan integration remains deferred.
