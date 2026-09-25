# SSH over Bluetooth for AsteroidOS

Bluetooth tunnel that gives SSH access to an AsteroidOS smartwatch without USB or Wi-Fi. Supports two transports: classic Bluetooth (L2CAP, fast) and BLE GATT (slower, wider compatibility).

```
┌─────────────┐  BT (L2CAP   ┌──────────────────┐  TCP
│  Host PC    │  or BLE GATT)│  Watch (hoki)    │──────► localhost:22
│ ble-ssh-    │◄────────────►│  ble-ssh-watch   │       (dropbear/sshd)
│ client      │              └──────────────────┘
│ :2222 TCP   │
└──────┬──────┘
       │
  ssh -p 2222 root@localhost
```

## Transports

| Transport | Protocol | Speed | Pairing Required |
|-----------|----------|-------|------------------|
| Classic BT | L2CAP BR/EDR (PSM 0x1001) | Workload-dependent | Yes |
| BLE | Framed GATT notifications + application ACKs | Not yet measured for this protocol | See below |

The watch daemon defaults to BLE. Set `BLE_SSH_TRANSPORT=both` to listen on both transports simultaneously.

## BLE GATT Service

UUIDs derived via UUID v5 (no hardcoded values):

| Item    | UUID                                         |
|---------|----------------------------------------------|
| Service | `a680eac3-dba3-5b1e-828c-2a6bb74cfc59`      |
| RX      | `47882c01-1d57-51a2-b858-3a840452aaa1`      |
| TX      | `146dddf7-595c-591a-8b1a-355a13959e15`      |
| ACK     | `2173f118-53de-578d-8f97-1cab46ebcf12`      |

- **RX** = host writes framed requests; the watch responds after forwarding the payload to SSH.
- **TX** = watch sends framed notifications through a peer-specific connection.
- **ACK** = host acknowledges each TX frame after writing its payload to the local TCP socket.

The revised protocol requires rebuilding/updating **both daemon and host client**.
The host checks for TX notification, RX request-write and ACK command-write
capabilities and refuses an incompatible older daemon. Classic L2CAP remains a
raw SSH stream.

### BLE framing and flow control

Each RX/TX frame begins with an eight-byte little-endian packet ID. The remaining
bytes are SSH data. RX starts with header-only ID 0 (START), followed by sequential
IDs beginning at 1. START reserves the daemon's slot before the host acquires
notifications; only then does the daemon open SSH. The host retries an explicitly
busy START for up to two seconds, without subscribing or consuming TCP data.
Host and daemon must be updated together for this reservation handshake.
A header-only frame after START means EOF. TX IDs increase
across sessions within one GATT registration; its header-only frame means EOF.

TX sends one frame at a time and waits for an application ACK carrying its
eight-byte ID. The host sends that ACK only after writing the packet to TCP,
including an ACK for EOF. ACK uses write-without-response
so it can proceed while a slow RX write request is pending. The daemon validates
its active peer and expected ID. A missing acknowledgment terminates the session
after 30 seconds rather than accumulating a backlog.

RX data uses write-with-response, a bounded daemon queue, and responds only after
the local TCP write completes. START is acknowledged when its reservation is accepted. Packet sizes reserve ATT overhead and the frame header
from the negotiated MTU. Both directions are polled independently so backpressure
in one direction does not block progress in the other.

The tunnel itself does not require BLE pairing, but the watch's other services
can trigger authentication during host discovery (observed with its battery
service). Pair the host and watch first when using the normal watch image, and
confirm the matching passkey on both devices.

The daemon accepts one active BLE peer and uses a separate notification writer
for each subscriber, so another subscriber cannot replace the active writer.
Requests from other peers are rejected before entering the active session's queue.
Incomplete setup is cleared after 30 seconds. Active BLE transfer errors clear
the session while retaining the GATT registration, avoiding service-cache churn
on peers. This requires the watch-side BlueZ CCC writer-reacquisition fix included
in the custom image layer. Registration, setup and BlueZ lifecycle failures can
still rebuild GATT. Classic errors also retain the registration. EOF closes the transport session; TCP
half-close is not used to keep an SSH transport alive. One SSH session runs at a time.

An active BLE delivery timeout also disconnects that peer before reopening
admission, with a five-second bound on the disconnect request. This releases
notification ownership retained by a suspended central application. BlueZ's
device disconnect can close other Bluetooth bearers to that same peer; it does
not change WiFi settings or restart the Bluetooth service.

The daemon watches BlueZ's D-Bus owner and rebuilds registrations after a restart,
including a rapid crash/restart that leaves the adapter powered throughout.

Tests cover byte preservation at multiple MTUs, acknowledgment ordering/timeouts,
backpressure, sequence errors, session teardown and pending reconnects. On Hoki,
BLE has passed a 32 KiB binary round trip, repeated SSH connections, recovery after
BlueZ restart and Bluetooth power cycling, and operation with Wi-Fi connected.
Classic L2CAP has passed a 256 KiB binary round trip, repeated SSH connections,
and recovery after BlueZ restart and Bluetooth power cycling. Auto mode has passed
a 64 KiB round trip through classic. The final build also passed the transfer and
reconnect matrix after all four restart/power-cycle recovery checks.
An abrupt LE disconnect while BR/EDR is connected can still make the first SSH
reconnect fail; a later connection through the same client succeeded. Captures
show physical link timeouts and a MIC failure during this forced recovery case;
the underlying cause remains open (BT-12 in the ledger).
Multi-central isolation still needs a second Bluetooth controller for a hardware test.

Dependency defects, compatibility limits, and unconfirmed upstream candidates are
tracked in the [Bluetooth upstream issue ledger](../../../knowledge/bluetooth-upstream-issues.md).

## Quick Start

### Include in our custom image

The recipe lives in `meta-nereid`, independently of the custom UI. Build the
ARM payload from the repository root, then use the normal image build:

```sh
bash meta-nereid/build-ble-ssh.sh
bash meta-nereid/tools/build-hoki.sh
```

`HOKI_BLE_SSH=1` is the default image selection; set it to `0` to omit the
package. `HOKI_CUSTOM_UI=0 HOKI_BLE_SSH=1` includes the tunnel with the stock UI
and the other policies in our custom layer. Payload fingerprints reject stale
binaries before the remote image build.

The packaged service is **disabled by default**. Enable it during the existing
local [personalization step](../../tools/README.md#initial-image-personalization)
with `--enable-ble-ssh`, explicit SSIDs and the stable SSH host key.
This also enables ConnMan Bluetooth at boot. No personal credentials enter the
build layer. Alternatively, on the watch use `systemctl enable --now ble-ssh-watch`
and enable Bluetooth through Settings or `connmanctl enable bluetooth`.

Runtime configuration is in `/etc/default/ble-ssh-watch`:

- `BLE_SSH_TRANSPORT=ble`: `ble`, `classic`, or `both`.
- `BLE_SSH_NAME=AsteroidOS-SSH`: advertised name, 1–20 UTF-8 bytes.
- `BLE_SSH_PORT=22`: local SSH port (always loopback).
- `BLE_SSH_GATT_HANDLE=0x1000`: first handle of the reserved eight-handle GATT
  service range. Reserve it exclusively for one daemon instance per controller
  and keep it stable across restarts and image updates. Verify a replacement
  range is unused: BlueZ 5.84 does not reject every possible overlap. The daemon
  never requests automatic renumbering.
- `RUST_LOG=info`: daemon log level.

Restart `ble-ssh-watch` after editing. The daemon never powers the radio or
changes global discoverability. It waits while Bluetooth is off and retries
registration after radio/service interruptions. Classic clients should use the
known address; enable discovery/pairing through Settings when needed.

The watch build includes a small dbus-crossroads patch that sorts ObjectManager
paths. Together with the reserved handles, this keeps characteristic identities
stable across BlueZ restarts. BlueZ 5.84 also requires that insertion order when
assigning the notification descriptor; see BT-07 and BT-10 in the issue ledger.

Both Rust binaries use the shared patched [`bluer` dependency](https://github.com/0x53A/bluer/tree/a07deae3b3f8d9c00ba965631dd693d2844fb45f/bluer/PATCH.md).
It corrects socket connection readiness, typed D-Bus event filtering, notification
subscription ordering, and missing incoming write metadata. Keep both Cargo patch
entries when moving this project into its own repository. The image payload
fingerprint includes the dependency sources.

### Deploy to watch

```sh
cd watch-rs
nix-shell --run ./deploy.sh        # via SSH (WiFi mode)

# Or via ADB (BT mode):
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'
nix-shell --run 'patchelf --set-interpreter /lib/ld-linux-armhf.so.3 --set-rpath /usr/lib:/lib target/armv7-unknown-linux-gnueabihf/release/ble-ssh-watch'
adb push target/armv7-unknown-linux-gnueabihf/release/ble-ssh-watch /tmp/ble-ssh-watch
adb shell 'cp /tmp/ble-ssh-watch /usr/bin/ble-ssh-watch && chmod +x /usr/bin/ble-ssh-watch && rm /tmp/ble-ssh-watch && systemctl restart ble-ssh-watch'
```

### Build and run host client

For reliable switching between classic and BLE on a dual-mode watch, enable
experimental APIs in the host's `bluetoothd` configuration (`--experimental`).
The client prefers `org.bluez.Bearer.LE1.Connect`, then
`Adapter1.ConnectDevice` with an explicit LE address type. Older BlueZ versions
without either API use generic `Device1.Connect`, which may select classic
instead of LE. The client reports this limitation rather than disconnecting an
existing classic session. Reusing an existing live LE connection does not require
these experimental methods.

For persistent NixOS setup, import [`host/nixos.nix`](host/nixos.nix) into the
host configuration and apply it through your normal system update. It selects
the locally patched BlueZ 5.86 package and sets
`hardware.bluetooth.settings.General.Experimental = true`. The version guard
requires an explicit patch rebase before a BlueZ upgrade. On other systemd
Linux hosts, set `Experimental = true` in the `[General]` section of
`/etc/bluetooth/main.conf` and restart Bluetooth. This enables the userspace
D-Bus APIs; kernel experimental features are not required. The temporary test
override from task 0161 disappears at reboot and is not persistent setup.
The host patches in [`host/patches`](host/patches) provide per-bearer discovery
readiness and bounded Service Changed cache cleanup. Experimental APIs alone
do not include those fixes; unpatched hosts retain the compatibility fallback.

For Hoki, the tested host also needed a longer LE supervision timeout. A captured
transfer failed with a controller timeout at the default 420 ms. The following
reversible profile requests a 45 ms interval, zero peripheral latency and a
five-second supervision timeout for **one paired watch**:

```sh
sudo python3 host/configure-link-timing.py --adapter HOST_MAC --device WATCH_MAC
```

Run this from `ble-ssh/`, replacing the addresses with the host adapter and watch
addresses. The helper briefly stops host Bluetooth, preserves the pairing-key
sections and saves the previous connection-parameter section for rollback.
Append `disable` to restore it. This writes BlueZ storage, but **does not ensure
the timing remains effective**: later recovery tests negotiated420ms after initial
5-second connections. Kernel auto-connect removal discards effective per-peer
timing. A first host repair had a collateral-cache flaw; a revised draft is kept
for later review and is **not selected by the default host package**.
See the [laptop findings](../../../_Tasks/0184_Bluetooth_Remaining_Failures/LAPTOP-FINDINGS.md).
Laptop repair/upstreaming is deferred in favor of watch-side resilience with
unpatched peers. The timing helper is not a complete recovery or MIC-failure fix.


```sh
cd host
./build.sh

# Classic BT (fast, requires pairing):
./target/release/ble-ssh-client --addr E4:A8:DF:3C:21:13 --transport classic

# BLE (pairing may be required by the adapter/security policy):
./target/release/ble-ssh-client --transport ble

# Auto (tries classic, falls back to BLE):
./target/release/ble-ssh-client
```

Without `--name`, discovery matches the advertised SSH service UUID, so the
default `AsteroidOS-SSH` name and custom names both work. An explicit `--name`
selects by advertised name substring instead; use `--addr` for a known device.

The host build helper targets Linux x86_64 and atomically publishes the binary
at `host/target/release/ble-ssh-client`, avoiding stale executables when a Cargo
default target changes the build output directory.

### Pair for classic BT (one-time)

```sh
bluetoothctl pair E4:A8:DF:3C:21:13
```

### Connect via SSH

```sh
ssh -p 2222 root@localhost
```

## Host CLI Options

```
--port <PORT>           Local TCP port [default: 2222]
--name <NAME>           Filter by advertised name substring (default: service UUID)
--addr <ADDR>           Connect by MAC address (skip scan)
--timeout <SECS>        Scan timeout [default: 15]
--transport <TRANSPORT> classic, ble, or auto [default: auto]
```

## Monitoring

```sh
# Via ADB (BT mode)
adb shell journalctl -u ble-ssh-watch -f

# Via SSH over BT tunnel
ssh -p 2222 root@localhost journalctl -u ble-ssh-watch -f
```

Library dependencies use pinned GitHub revisions in Cargo.toml and Cargo.lock.
Cargo fetches the bluer and dbus-rs forks, including the required fixes; no
nested submodule initialization is needed.
