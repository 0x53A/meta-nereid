# Haptics

Small Slint explorer for Hoki's PM660 ERM vibrator. Authored by Lukas Rieger
<code@lukasrieger.com>.

The pulse grid varies **pulse cadence**, not motor/carrier frequency. Columns
are 1, 2 and 4 pulses/second; rows are 100%, 65% and 35% requested drive level.
Each tap sends 1, 2 or 4 pulses in a one-second window, each 80 ms long. Lower
drive levels may feel weak or fail to spin up; percentages are voltage requests,
not calibrated perceptual strength. The Effects page exposes all six device-tree
presets with the same three strength choices. IDs 2 and 4 are identical in the
selected device tree. Labels describe intended patterns, not measured sensations.

Stop interrupts playback. New requests remain disabled until cleanup completes.
Closing the window also requests stop and joins the worker. There is no looping
background playback. Desktop `--preview` never opens an input device.

## Build and preview

From this directory:

```sh
nix-shell --run 'cargo test --locked -p hoki-haptics'
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh ../target/armv7-unknown-linux-gnueabihf/release/hoki-haptics'
nix-shell --run 'cargo run --locked -- --preview'
```

Renderer capture (requires a desktop display or Xvfb):

```sh
nix-shell --run 'cargo run --locked --features capture -- --preview --capture /tmp/haptics-grid.png'
nix-shell --run 'cargo run --locked --features capture -- --preview --effects --capture /tmp/haptics-effects.png'
```

Registered in the shared workspace and runtime image inventory. The image builder
installs the binary, launcher and desktop entry. For an authorized manual deployment,
install the binary to `/usr/lib/hoki-haptics`, executable launcher to
`/usr/bin/hoki-haptics`, and desktop entry to
`/usr/share/applications/hoki-haptics.desktop`. Run the UI as **ceres**.

## Control path and limits

`src/evdev.c` discovers the input device by the exact name `qti-haptics` and
checks its advertised FF capabilities. It opens read/write, uploads with
`EVIOCSFF`, writes `EV_FF` start/stop events, and erases with `EVIOCRMFF`.
Linux target headers provide the correct ARM32/native structure layout.
The UI shows errors rather than treating an unavailable device as success.
The worker prints full errors to stderr; successful submission is not proof of
physical motion.

Constant pulses use `FF_CONSTANT`, replay length and positive level in
`1..32767`. Built-in effects use `FF_PERIODIC`/`FF_CUSTOM` and three writable
`int16_t` values: preset ID, returned seconds and returned milliseconds. This
driver's custom payload selects a preset; it does not accept uploaded samples.
The returned duration governs the wait; unsupported durations are rejected.

One effect is uploaded before each play, with no slot cache, no global gain and
no overlap within the app. Early stop waits out the original constant timer
before erase, addressing the known stale-timer behavior for this app's sequence.
The driver shares playback state across clients: simultaneous ngfd notifications
can still interfere. An evdev grab would not provide FF output exclusivity.
The app does not change or stop ngfd. Controlled on-watch comparison should use
a quiet feedback period; if interference occurs, coordinate a temporary ngfd pause
and restore its previous state. Input permissions must allow ceres to open the
node read/write (the image's existing input rules use group `system`). Do not
run the GUI as root to bypass a permission failure.

Deployed 2026-09-28: qti-haptics is present at event2, ceres has the required
group access, and the app starts fullscreen in the user session. Physical
sensations, Stop behavior on hardware and touch ergonomics await user testing.
