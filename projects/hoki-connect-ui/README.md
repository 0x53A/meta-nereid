# Connect for Hoki

Native Slint Device and Music screens for the 416 × 416 circular watch. Uses the
local `hoki-connect` daemon; the GUI never handles TLS keys or connects to a peer.

- Device: live connection, local pairing request, ping and incoming ping notices.
- Music: select a player using the tall left/right edge controls or swipe across
  the center text (left for next, right for previous), now playing, play/pause,
  previous/next, and player volume in 5% steps. Controls follow peer capabilities.
- On Music, rotate the crown to change player volume in 1% steps (one raw crown tick per step). Clockwise increases it; counterclockwise decreases it.
  Device/offline/unknown-volume states ignore rotation. Fast rotation never
  creates a delayed command backlog. Volume updates immediately on screen; the
  latest absolute target is sent at most every 100 ms, independently of action busy state.
  Older snapshots are held off for two seconds after the latest input, then peer
  state wins (including a rejected or externally changed volume).
- Bottom tabs switch screens. Top hardware button closes the app; bottom button
  switches screens when delivered by the compositor. No long press is required.
- Offline, unpaired, waiting for approval and no-player states have distinct copy.

## Build and deploy

Run in this directory, using its own Nix shell:

```sh
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-connect-ui'
scp target/armv7-unknown-linux-gnueabihf/release/hoki-connect-ui root@hoki.local:/usr/lib/hoki-connect-ui
scp deploy/hoki-connect-ui root@hoki.local:/usr/bin/hoki-connect-ui
scp deploy/hoki-connect-ui.desktop root@hoki.local:/usr/share/applications/hoki-connect-ui.desktop
```

Requires the matching daemon with MPRIS and local `snapshot` support from
[../hoki-connect](../hoki-connect/README.md). Run both as **ceres**. The launcher
entry is **Connect**. As root, a manual launch is
`su -s /bin/sh ceres -c /usr/bin/hoki-connect-ui`.
This app is not yet included in full image packaging.

## Reproducible visual review

```sh
nix-shell --run 'cargo run -- --preview music --capture /tmp/connect-music.png'
```

Preview choices: `device`, `music`, `offline`, `pair`, `pairing`, `empty`, `long`, `ping`, `notice`.
These are explicitly synthetic states; preview mode never contacts the daemon.
Crown movement changes only the synthetic volume in preview mode.
Capture uses the actual Slint software renderer. It requires a desktop display.
`--capture` alone captures the live GUI and exits. `HOKI_CONNECT_STATE` overrides
the daemon state directory for isolated testing.

Read [DESIGN.md](DESIGN.md) for the reusable layout specification.

## Validation

Built and deployed on Hoki; real evdev touch tests exercised tabs, player
selection, ping, playback, previous/next and volume through stock KDE Connect.
A silent temporary MPRIS player confirmed the actions without affecting real
playback. Incoming ping display, daemon-loss state and reconnect were checked.
See [task 0208](../../../_Tasks/0208_Connect_GUI/summary.md) for captures and evidence.
