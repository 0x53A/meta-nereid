# Hoki Connect

A small Rust client speaking the native KDE Connect LAN protocol (version 8).
Pairs directly with KDE Connect, exchanges pings and battery status, controls remote
media players through MPRIS, provides read-only SFTP browsing, and receives files.
The round-screen frontend lives in
[../hoki-connect-ui](../hoki-connect-ui/README.md). No custom desktop service or
SSH bridge is required.

The transport connects simultaneously to up to 16 explicitly configured companions
(LAN or Tailscale IP), each with its own pinned certificate, pairing, reconnect
loop and media state. A shared watch identity appears as Hoki on each companion.
The GUI's Add device action opens a 60-second LAN discovery window: the watch
announces Hoki over UDP, lists nearby companions, and accepts temporary incoming
TCP connections. Outside that window it only makes configured outgoing
connections. mDNS discovery is not implemented. Selecting a device changes the
control target without disconnecting other companions.

The earlier single-companion version was verified on Hoki against stock laptop
KDE Connect 26.08.1: user-approved pairing,
ping in both directions (including a desktop notification), and service restart
with the same identity and pairing. The ARM release binary is approximately
4.2 MiB.

## Build

From this directory:

```sh
nix-shell --run 'cargo test'
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh ../target/armv7-unknown-linux-gnueabihf/release/hoki-connect'
```

OpenSSL is statically linked into the application. No KDE Frameworks, Qt or
additional TLS shared libraries are needed. Cargo.lock pins dependencies.

## Battery and receiving files

Paired companions receive watch battery percentage and charging state on connection,
on request, and when a once-per-minute sample changes. Readings come from
`/sys/class/power_supply/battery/{capacity,status}`; missing or invalid readings
are not invented. A low-battery threshold event is sent when entering 15% or lower
while discharging. Received companion battery state appears in the selected
device's snapshot and UI, only while paired and connected.

Use KDE Connect's **Send files** on the phone or computer to receive files into
`/home/ceres/Download` (`$HOME/Download` for development). The daemon receives
files while the app is closed; the open app shows the selected companion's latest
transfer result. It does not open or extract files. Read-only SFTP can retrieve
them later. Text and URL sharing are not implemented.

Only paired sessions can submit transfers. Payload TLS verifies the paired
certificate and connects only to the control connection's IP and a KDE Connect
payload port (1739–1764). Disconnecting or unpairing cancels queued/active work.
Names must be visible leaf names of at most 180 UTF-8 bytes: paths, separators,
leading dots, trailing dots, colons, outer whitespace, controls and bidi overrides
are rejected. Percent escapes remain literal characters; nothing is URL-decoded.
The receiver opens the fixed Download directory without following a symlink,
anchors all writes to its directory descriptor, creates private temporary files,
and publishes complete files atomically without replacing existing entries.
Collisions use `name (1).ext`, `name (2).ext`, etc. Files have mode 0600.
Failed/cancelled transfers remove their temporary files; abrupt process termination
or power loss can leave hidden `.connect-*.part` files, never completed downloads.

Limits: 512 MiB per file, 64 MiB free-space reserve, two active transfers globally,
eight queued per companion, 10-second socket timeouts and 30 minutes per payload.
Saturation and invalid metadata produce a failed-transfer result without dropping
the companion's control connection. `HOKI_CONNECT_DOWNLOAD_HOME` and
`HOKI_CONNECT_BATTERY_DIR` override local fixture paths for tests.

Ping is a message; Find is a separate ringing protocol. Find and notification
mirroring (which needs filtering), along with the other proposed protocols,
remain deferred.

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

On the watch, open **Connect → Device → Add device**. Keep both devices on the
same Wi-Fi and open/refresh KDE Connect on the companion. Either:

- Select Hoki on the phone and request pairing. Select the phone on the watch,
  compare the eight-character verification codes, then tap **Accept pairing**.
  **Reject** declines the request without saving trust.
- Select a discovered companion on the watch and tap **Connect device**, then
  **Pair device** once connected. Compare the code shown on the watch with the
  phone before accepting there.

Visibility ends after one minute. Search starts another window. Pairing requests
get a full 25 seconds for incoming approval, even when visibility ends; outgoing
requests wait 30 seconds. Finish a pending request before starting another search. Discovery alone
never grants trust. Incoming approvals use a random connection-specific token;
certificate/device-ID and protocol checks precede displaying the request. The
outgoing flow saves an unpaired certificate pin after local selection; feature
packets remain disabled until explicit pairing succeeds. Existing pins are never
replaced. The UI discovers IPv4 LAN companions; routed/Tailscale addresses still
use the explicit CLI configuration below. Incoming connections use the advertised
companion port when available, otherwise KDE Connect's default port 1716. Enrollment
hands the established TLS connection to the normal peer worker instead of reconnecting.
Late advertisements refresh the reconnect port before enrollment. Configured unpaired
devices remain visible for retry, including after restart or remote unpair.

Keep the existing companion and add another, as **ceres**:

```sh
/usr/lib/hoki-connect add PHONE_IP:1716 PHONE_DEVICE_ID PHONE_SHA256_FINGERPRINT
/usr/lib/hoki-connect devices
/usr/lib/hoki-connect pair PHONE_DEVICE_ID
# Accept Hoki in the phone's KDE Connect before its approval timer expires.
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

`unpair` clears the selected companion’s local trust and notifies it. Incoming pairing requests on
configured sessions show a code and explicit Accept/Reject controls in the app. New companions can
request pairing during Add device and require on-watch approval. A timed-out request
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

## Browse and download watch files

On a paired computer, select Hoki in KDE Connect and choose **Browse device**
(or open Hoki from Dolphin's devices list). The watch exposes `/` read-only,
including all files the `ceres` service account can read. Normal filesystem
permissions still apply; root-only files remain inaccessible. This includes
hidden files and app data readable by ceres, so grant pairing only to computers
you want to have that access. Copy files from the watch to download them.
Uploads, deletion, renaming and other filesystem modifications are refused.

The native `kdeconnect.sftp.request` / `kdeconnect.sftp` exchange starts a
per-peer SSH listener on the local address of the paired TLS connection, using
an available port in 1739–1764. A random password travels only over that TLS
connection; it is not stored in snapshots or logs. SSH connections must originate
from the same peer IP. Each connection permits only the SFTP subsystem; there
is no shell, command execution, forwarding or account login. The subsystem runs
as ceres using OpenSSH `sftp-server -R -d /`.

Unpairing or losing the KDE connection closes the listener and active transfers.
Reconnect and browse again to obtain fresh credentials. Transfers are streamed,
with at most four SSH connections per peer and one SFTP channel per connection.
The daemon refuses to start file access as root.

The watch uses `/usr/libexec/sftp-server`, already present on the inspected image;
the image recipe also explicitly depends on `openssh-sftp-server`. A local
`HOKI_CONNECT_SFTP_SERVER` environment override supports host testing; the Nix
development shell supplies its OpenSSH path. No changes to the system SSH server,
authorized keys, user passwords or privileges are needed.

Compatibility follows the upstream [KDE SFTP plugin](https://github.com/KDE/kdeconnect-kde/tree/master/plugins/sftp).
Verified on the watch through stock desktop KDE Connect/SSHFS: root listing and
file download with a matching checksum. Some SSHFS versions block absolute or
parent-relative symlinks by default; use the target's direct path in those cases
(for example `/usr/lib/os-release` instead of `/etc/os-release`).

## Media and local GUI interface

`refresh` requests the player list and selected player metadata. `next-player`
cycles the available players. `play-pause`, `next`, `previous`, `volume-up` and
`volume-down` send only the corresponding allowlisted KDE Connect MPRIS requests.
Volume is the selected player's volume, bounded to 0–100%, in 5% steps. Playback
controls are gated by the peer's reported capabilities. No arbitrary MPRIS method,
remote command or artwork download is supported. Pulling files from the watch uses
the separate read-only SFTP provider; incoming files use KDE Connect Share.

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
