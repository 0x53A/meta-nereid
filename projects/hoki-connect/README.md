# Hoki Connect

A small Rust client speaking the native KDE Connect LAN protocol (version 8).
Pairs directly with KDE Connect, exchanges pings, and controls remote media
players through its native MPRIS plugin. The round-screen frontend lives in
[../hoki-connect-ui](../hoki-connect-ui/README.md). No custom desktop service or
SSH bridge is required.

The transport connects simultaneously to up to 16 explicitly configured companions
(LAN or Tailscale IP), each with its own pinned certificate, pairing, reconnect
loop and media state. A shared watch identity appears as Hoki on each companion.
It does not implement broadcast/mDNS discovery or listen for incoming TCP
connections. Selecting a device changes the control target without disconnecting
other companions.

The earlier single-companion version was verified on Hoki against stock laptop
KDE Connect 26.08.1: user-approved pairing,
ping in both directions (including a desktop notification), and service restart
with the same identity and pairing. The ARM release binary is approximately
4.2 MiB.

## Build

From this directory:

```sh
nix-shell --run 'cargo test'
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-connect'
```

OpenSSL is statically linked into the application. No KDE Frameworks, Qt or
additional TLS shared libraries are needed. Cargo.lock pins dependencies.

## Configure and pair

Obtain the laptop device ID using `kdeconnect-cli --my-id` and its public
certificate fingerprint using:

```sh
openssl x509 -in ~/.config/kdeconnect/certificate.pem -noout -fingerprint -sha256
```

The fingerprint is an explicit trust anchor copied over the existing trusted
watch SSH connection. Do not copy the laptop's private key. On the watch, run
as **ceres**, never root:

```sh
/usr/lib/hoki-connect init LAPTOP_IP:1716 LAPTOP_DEVICE_ID SHA256_FINGERPRINT
/usr/lib/hoki-connect serve
```

The daemon must stay running. From another ceres session:

```sh
/usr/lib/hoki-connect pair
# Accept Hoki in laptop KDE Connect within 25 seconds.
/usr/lib/hoki-connect status
/usr/lib/hoki-connect ping
```

### Add a phone or another computer

Keep the existing companion and add another, as **ceres**:

```sh
/usr/lib/hoki-connect add PHONE_IP:1716 PHONE_DEVICE_ID PHONE_SHA256_FINGERPRINT
/usr/lib/hoki-connect devices
/usr/lib/hoki-connect pair PHONE_DEVICE_ID
# Accept Hoki in the phone's KDE Connect within 25 seconds.
/usr/lib/hoki-connect select PHONE_DEVICE_ID
/usr/lib/hoki-connect ping
```

Obtain each companion's device ID and certificate fingerprint through a trusted
channel, as for the initial companion. `add` never replaces an existing device
or certificate pin. It loads the new companion into the running daemon without
restarting the existing sessions; with the daemon stopped, it saves configuration
for the next start. This requires the updated daemon; an older daemon must first
be upgraded and restarted.

`devices` and `status` query the live daemon and return the selected companion's
snapshot plus a `peers` list with each companion's connection and saved trust.
`trusted` records persistent pairing even when disconnected; `status.paired`
describes the current authenticated session. `select DEVICE_ID` persists the
control target. `next-device` and `previous-device` cycle configured companions,
including offline ones. Actions such as `pair`, `unpair`, `ping` and media controls
accept an optional device ID; omitting it addresses the selected device. Offline
actions fail rather than waiting for a future reconnection. `unpair DEVICE_ID`
requires a live connection and affects only that companion.

The GUI's Device page has left/right controls when multiple companions exist.
The Music page names the selected device. Other companions remain connected and
retain independent media/player state.

Send a reverse ping with `kdeconnect-cli --name Hoki --ping-msg 'Hello from laptop'`.
Received messages appear as JSON in the daemon journal and in `last-ping.json`.
The Connect GUI displays new pings while open; there is no background notification
or wake integration.

`unpair` clears the selected companion’s local trust and notifies it. Incoming pairing requests are
rejected; initiate from Hoki so local consent is explicit. A timed-out request
can be retried with `pair`. Commands are not carried across network reconnects.

`deploy/hoki-connect.service` runs the daemon as a ceres user service. Install
the binary at `/usr/lib/hoki-connect` and the unit in the systemd user unit path,
then use `systemctl --user -M ceres@ enable --now hoki-connect.service` as root.

## State and compatibility

Identity, configuration and the local command socket live in
`~/.config/hoki-connect` (directories 0700, files 0600). Existing single-companion
`config.json`, `paired.json` and identity remain in place and continue working
without re-pairing. Additional companions store config, trust, status, media and
ping state in `peers/DEVICE_ID/`. `selected.json` stores the control target; the
original companion is the default when no selection has been saved. Preserve
the entire directory for backup/restore. Changing or removing existing companion
configuration requires an explicit offline review and daemon restart; live reload
only adds companions. For isolated validation, tests or desktop experiments
can override this with `HOKI_CONNECT_STATE`. Never publish `identity.json`: it
contains the watch's private key. Preserve this directory if keeping pairing
across a reflash matters. The daemon, user service and UI are included in the
custom Hoki image. The service starts only when a peer configuration exists;
private identity and pairing state must still be preserved and restored locally
when flashing and are never included in the reusable layer.

Private-state saves create an exclusive per-save temporary in the destination
directory, write and sync it, then rename it over the target. Leftovers from an
interrupted save do not block later saves and are not removed by another save.
Failure cleanup removes only the current save's temporary. Files remain mode
0600. Publication is atomic per file; this does not make updates across multiple
state files transactional or guarantee directory durability after power loss.

The peer certificate is pinned during TLS, its CN must match the expected device
ID, and the encrypted identity must still match protocol version 8 and device ID.
A changed certificate fails closed and needs an explicit configuration review.
Ping and MPRIS client capabilities are advertised. Feature packets are ignored until paired.
Packets and partial-frame duration are bounded; malformed input disconnects.

Network loss retries every five seconds; each idle connection waits in
poll and wakes for local commands. Suspend/wake and battery costs are not yet
validated. The pinned address must be reachable and allowed by the host firewall
and tailnet policy.

Protocol references: KDE/kdeconnect-kde **v26.08.1**, core/backends/lan/
lanlinkprovider.cpp, core/backends/pairinghandler.cpp and core/deviceinfo.h.

## Media and local GUI interface

`refresh` requests the player list and selected player metadata. `next-player`
cycles the available players. `play-pause`, `next`, `previous`, `volume-up` and
`volume-down` send only the corresponding allowlisted KDE Connect MPRIS requests.
Volume is the selected player's volume, bounded to 0–100%, in 5% steps. Playback
controls are gated by the peer's reported capabilities. No arbitrary MPRIS method,
remote command, file transfer or artwork download is supported.

The private UNIX control socket accepts those commands and `snapshot` (write a
command then half-close the write direction). Commands return `queued`, `busy`, `offline`, or an `error:` reply;
this is queue acknowledgement, not peer acknowledgement. `snapshot` returns
the selected device’s status, public peer name, bounded media metadata, latest
ping and last sent ping metadata, plus `selected_peer` and a `peers` summary list. It never returns identity keys. A GUI socket failure means offline;
it must not rely on a stale status file to claim a live connection. Commands
queued during disconnection or TLS handshake are discarded on reconnection.
Local accept failures are logged and retried after one second, avoiding a busy
loop on persistent errors. Interrupted accepts retry immediately. This backoff
does not change accepted connections or the separate network reconnect delay.

The GUI crown uses private socket command `volume-adjust:N` for signed integer
percentage adjustments (-100…100 excluding zero), bounded to the player's 0–100%
volume range. Touch `volume-up`/`volume-down` retain their 5% steps. This requires
the matching updated daemon and GUI; the command does not accept arbitrary methods.

Volume readback reconciles the last requested integer with KDE MPRIS’s float
rounding/truncation echo (for example 51% may be reported as 50%). A different
reported value clears that hint, as does changing players. An external change
to that exact ambiguous one-percent-lower value cannot be distinguished from
the echo until another value is reported. The hint is not persisted.

Responsive volume uses `volume-set:{"player":"name","volume":70}` on the private
socket. Targets are integers 0–100 and must match the selected player. The daemon
keeps one replaceable volume target per companion, clears it on reconnection, and wakes its TLS
loop through a socket pair when local commands arrive. Idle waiting uses poll,
not a high-frequency network-read timeout. Volume send/receive logs include
wall-clock milliseconds for latency investigations.

For race-safe routing, the socket also accepts a JSON envelope such as
`{"peer_id":"PHONE_DEVICE_ID","command":"ping"}`. This includes volume commands;
the UI binds pending volume to both device and player. Unaddressed commands
remain compatible and use the selected device. The socket's `select:DEVICE_ID`
changes selection and `reload` loads newly added configurations. Snapshots expose
no private keys or certificate material.

Local `cargo test` includes two synthetic TLS companions connected to the real
daemon, covering legacy trust, live addition, addressed ping/volume, independent
reconnection/unpairing and persisted selection after restart. Real Android and
watch power/suspend behavior still require device validation.
