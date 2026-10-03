# Native Hoki endpoint

The `nereid-pict` daemon in this repository supplies local Nereid output
management, desktop-seat uinput and Venus capture/encoding to Pict's generic
host library. It disables Pict's desktop feature and has no desktop UI, D-Bus,
PipeWire or FFmpeg dependency. The optional `dev-ssh` feature runs this adapter
on a PC using the development bridge; it is never part of the Pict desktop host.

The shared library owns signed pairing, saved devices and grants, session/grace
lifecycle, the owner-only control socket, HTTP/WebSocket routes, and str0m
WebRTC. The watch calls `nereid-venus` over local pipes. No Python or SSH bridge
is used for capture, encoding or input. Rendering is requested only when the
capacity-one transport handoff has space; the watch backend allows one frame
request in flight. Hardware cleanup drains output and waits for Venus flush.

## Build and install

This is a standalone development crate, outside the image runtime workspace.
Its shared host and protocol dependencies are pinned to
[Pict revision `2716e049954d`](https://github.com/0x53A/pict/commit/2716e049954dd4a2812f13456e35857a7ba58524)
on GitHub. Cargo fetches them over SSH using your GitHub access; no local Pict
checkout is needed for the daemon. The adapter's own Cargo.lock fixes the Git
revision and resolved third-party versions. Image recipe integration remains
separate.

For PC development only, build with `--features dev-ssh` and set
`PICT_NEREID_SSH` and, if needed, `PICT_NEREID_HOST_KEY`. Install the Python
bridge and Venus worker following `tools/nereid-venus.md`. The adapter always
uses hardware H.264; the historical Python raw-frame helper is retained only
as development tooling. Default builds use local native capture and input.

Install Rust 1.97.1 with the `armv7-unknown-linux-gnueabihf` target. From this adapter directory:

```sh
PICT_WATCH_BUILD=/path/to/hoki/build \
PICT_WATCH_ELF_PATCH=/path/to/meta-nereid/patch-watch-elf.sh tools/build-watch.sh
# Uses the relocated Yocto SDK; also set NEREID_SDK_IMAGE=name@sha256:digest.
PICT_WATCH_BUILD=/path/to/hoki/build \
PICT_WATCH_ELF_PATCH=/path/to/meta-nereid/patch-watch-elf.sh tools/build-nereid-venus.sh
# Build web assets from the same pinned Pict revision, as described below.
```

The web client inputs are in the same Pict revision. Build them in the ignored
build directory using Pict's documented web-build environment:

```sh
# PICT_WATCH_BUILD must be set to your Hoki build directory.
git clone git@github.com:0x53A/pict.git "$PICT_WATCH_BUILD/pict-source"
git -C "$PICT_WATCH_BUILD/pict-source" checkout --detach 2716e049954dd4a2812f13456e35857a7ba58524
(cd "$PICT_WATCH_BUILD/pict-source" && nix-shell --run tools/build-web.sh)
```

Install `web/` from that checkout with the daemon. When updating the Pict
revision, update both Cargo dependencies, Cargo.lock and this web checkout pin
together.

Deploy the daemon to `/usr/bin/nereid-pict`, worker to
`/usr/libexec/pict/nereid-venus`, `web/` to `/usr/share/pict/web/`, and the service
`deploy/nereid-pict.service` to `/etc/systemd/system/nereid-pict.service`. Stop an existing daemon before
replacing it. `PICT_DATA_DIR` can override the asset root at runtime; the existing
compile-time setting remains a fallback. Install and start the service manually;
it is not automatically enabled for boot.

The initial service runs as root for ION/V4L2 and uinput access; desktop apps and
the compositor remain ceres-owned. Config/identity are private in
`/var/lib/nereid-pict/pict`, control socket in `/run/nereid-pict` (0700).
The future watch UI needs an intentional local approval interface; do not relax
these permissions merely to make the socket reachable.

Set `PICT_ORIGIN=https://WATCH-DNS-NAME` in `/etc/nereid-pict.env`, and configure
Tailscale Serve to proxy HTTPS to `http://127.0.0.1:8787`. HTTP deliberately binds
loopback only. Serve is tailnet-only, not Funnel. The tailnet policy must permit
TCP 443 and UDP 50000 to the watch from intended clients. `PICT_ICE_IP` optionally
selects a LAN address instead of the default Tailscale IPv4; `PICT_ICE_PORT`
selects a UDP port (service uses 50000 because there is one output; desktop
application defaults to an ephemeral port). No tailnet policy is changed by Pict.

Local control example:

```sh
XDG_RUNTIME_DIR=/run/nereid-pict nereid-pict --ctl status
XDG_RUNTIME_DIR=/run/nereid-pict nereid-pict --ctl accept REQUEST input
XDG_RUNTIME_DIR=/run/nereid-pict nereid-pict --ctl pin REQUEST PIN input
XDG_RUNTIME_DIR=/run/nereid-pict nereid-pict --ctl close SESSION
```

There is no tray or notification-daemon dependency. Pending requests stay denied
until the owner approves them; the existing PIN workflow is unchanged. Comparison
pairing, Slint enable/address/QR controls and launcher integration are follow-ups.

## Supported limits

One virtual output; watch controls stay on the built-in seat. Mouse and physical
keyboard forwarding use USB-identified uinput devices routed to the desktop.
Input release/revocation drops held state. Pen/touch injection, physical-display
mirroring, raw-Zstd and preview-only sessions are not supported by this backend.

Output is configured at 35 Hz. Venus requires even sizes, width 96..1920,
height 64..1088, within the existing 1080p pixel budget and integer 100%/200%
scale at admission. These are admission bounds, not performance guarantees.
The adapter currently creates at 100% scale using the client's initial dimensions.
It does not advertise display preferences or resizing, and rejects live changes.
Reconnect with new client dimensions to change the output. Default bitrate is 8 Mbps; live bitrate changes work. VAAPI quality,
low-power and buffer controls are not mapped to Venus and nondefault values are
rejected. RGB remains experimental and unused.

Unit checks:

```sh
nix-shell --run 'cargo test --locked'
nix-shell --run 'cargo test --locked --features dev-ssh'
```
