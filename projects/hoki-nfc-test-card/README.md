# Hoki NFC test card

A fixed, read-only NFC Forum Type 4 NDEF text application for nfcd. Its sole record
is the invented text `Hoki NFC test card`. It accepts no card dumps, payment data,
configurable AIDs or configurable command responses.

**The app is implemented; over-the-air operation on Hoki is not.** Our current
Linux NFC backend only advertises reader mode. The app detects that and exits
with status 3 before registering or requesting a mode change.

## Build and validation

From this directory:

```sh
nix-shell --run 'make all check'
```

Four protocol tests verify the fixed NDEF record, read bounds, rejected writes
and isolated/reset session state. Four integration tests launch the actual app
against a fake nfcd service on private D-Bus instances. They verify unsupported
hardware handling, registration/APDU callback signatures, read-only responses,
cleanup, caller authentication and daemon-loss shutdown. They do not emulate RF
or establish actual nfcd/controller interoperability.

An optional BitBake recipe lives in the custom Hoki layer. It builds and installs
`/usr/bin/hoki-nfc-test-card`; it does not enable a service or change the image's
NFC daemon selection. It is a command-line development tool, without a launcher UI.

## Usage on a compatible nfcd installation

```sh
hoki-nfc-test-card --check
hoki-nfc-test-card
```

The check only queries advertised adapter capabilities. The normal invocation
registers the NFC Forum NDEF application and requests card-emulation mode while
disabling reader and peer modes for the lifetime of that request. It does not
turn NFC on globally. Stop with Ctrl-C to release the request and unregister.
Registration is not evidence of an RF link; controller state and a physical read
must be checked separately. The normal system bus requires applicable nfcd policy
permissions. `--session` is reserved for isolated local testing.

The application checks the unique D-Bus identity of nfcd on incoming callbacks.
Each host has independent selection state, reset on restart/deselection. There
is no implicit application selection. The card exposes the standard capability
container and NDEF files and denies all writes.

## Why this does not yet work over NFC on Hoki

The exact pinned 4.14.206 source has three relevant limitations:

- The Hoki controller patch selects polling-only protocol mappings.
- The target-mode stack handles NFC-DEP/LLCP rather than ISO-DEP HCE.
- `nfc_tm_data_received()` feeds LLCP. The raw monitoring socket has no send
  operation; the connected sequence-packet socket is a reader-side transport.
  There is no existing userspace card-response channel to connect to nfcd.

A hardware implementation needs a supported card-side transport, controller
listen-mode validation and a compatible nfcd backend. This task does not add such
a transport, modify the kernel, or claim that a flag change would supply it.
See `_Tasks/0169_NFC_Test_Card/summary.md` for exact source evidence.
