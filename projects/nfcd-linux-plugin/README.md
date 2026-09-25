# Linux NFC backend for nfcd

Experimental reader backend using the kernel NFC generic-netlink family and
AF_NFC SOCK_SEQPACKET sockets. The kernel retains ownership of the NFC controller,
firmware setup and NCI protocol. This does not use the Android HAL or /dev/nq-nci.

Implemented capabilities:

- Initial adapter enumeration and device-added/device-removed events.
- nfcd power and reader-mode requests, with serialized worker operations.
- NFC-A Type 2 and ISO-DEP Type 4A/4B targets, using nfcd's tag implementations.
- Raw reader exchanges, nfcd request sequencing and cancellation, bounded
  exchange timeouts, kernel status-byte removal and socket-error handling.
- Technology filtering and conservative capability reporting.

Card emulation, peer-to-peer, NFC-F, ISO15693 and MIFARE Classic are not implemented.
The Linux MIFARE protocol bit does not distinguish Classic from Type 2; the backend
does not expose a Classic target as a Type 2 tag.

## Build and test

With installed nfcd development headers:

```sh
make
make check
```

With an nfcd source checkout, from this project directory:

```sh
nix-shell --run 'make NFCD_SRC=../_Tasks/0162_Neard_Feasibility/nfcd all check'
```

The source-checkout test also compiles nfcd's real target implementation and tests
it over local SOCK_SEQPACKET socket pairs. No test transmits NFC or requires root.
`make install DESTDIR=... PLUGIN_DIR=/usr/lib/nfcd/plugins` stages linux.so.

Adapter lifecycle tests additionally use a built nfcd core and libnfcdef static
library. After building those dependencies, pass `NFCD_CORE` (defaults to
`$(NFCD_SRC)/core/build/release/libnfc-core.a`) and `NFCDEF_LIB` to `make check`.
The adapter tests substitute only the kernel command transport; the adapter base
class and asynchronous backend state handling are real.

The optional BitBake recipe is in meta-nereid/recipes-nemomobile/nfcd. It does
not add nfcd to the default image or start it. The image build script stages this
project alongside the layers so the recipe can resolve its sources.

## Ownership and operating limits

Only one daemon/application may own the controller. The package supplies an nfcd
systemd conflict with neard; direct AF_NFC apps must also be stopped before using
nfcd. Installing the plugin is not an app migration: hoki-nfc currently accesses
the kernel directly and would still need a separate D-Bus migration.

Polling belongs to the netlink socket that starts it. Each adapter keeps that
control socket alive, and all its commands are serialized. Kernel socket cleanup
stops polling if the owner exits. A busy controller is reported as an error, not
forcibly reset. Failed operations stop automatic retries until a new state request.

Tag activation runs on a worker because kernel activation may wait for hardware.
Stale activations are closed after a mode/power change. Cancellation closes the
tag socket, sacrificing that activation so late replies cannot satisfy another
request. Shutdown requests best-effort power-off; abrupt process exit does not
guarantee physical power-off.

Idle tag disappearance relies on kernel TARGET_LOST/socket events. There are no
application-level presence probes, so an idle tag may remain listed until a later
exchange detects its removal. This needs hardware validation.

The Linux target API does not expose complete ISO-DEP activation parameters
(ATS/ATQB). The nfcd Type 4 wrappers use a conservative 256-byte frame-size hint;
the kernel/controller perform ISO-DEP framing. Missing activation data must not
be treated as measured card metadata. Real Type 4 interoperability is unvalidated.

This backend cannot repair a controller that requires a reboot or make kernel
ISO-DEP listen support appear. Hardware support is not established by unit tests.
