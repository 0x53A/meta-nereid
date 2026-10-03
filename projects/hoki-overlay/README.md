# Hoki overlay

Slint renderer for the compositor's optional, persistent `overlay` role. A mint
ring surrounds the 416×416 display without reducing app dimensions. The selected
watch configuration uses `--width 3`; the default remains two physical pixels.
Widths 1–8 are supported.

Clock activities come from `org.hoki.Clock1` on the session bus. Timers partition
the circumference, drawing their remaining fraction in amber (muted when paused).
A running stopwatch occupies a small amber arc. Alarm/timer expiry opens a dark
circular alert with Dismiss and, for alarms, Snooze for five minutes. Only alerts
claim touch input; the ordinary ring is transparent and passes input through.
The Clock daemon owns persistence, deadlines, display wake and vibration; see
[Clock](../hoki-clock/README.md). No generic notification server or watchface
notification-capability negotiation is implemented yet.

Slint renders into premultiplied ARGB8888 SHM via a minimal software backend,
using `zwlr_layer_shell_v1` Overlay. The client blocks on Wayland and eventfd
updates when idle; visible running timers redraw once per second, with BOOTTIME
elapsed calculations that include suspend. Managed visibility suppresses hidden
redraws. Closing the compositor's stdin pipe exits the process. Logs use stderr.

## Build and preview

Run from this directory:

```sh
nix-shell --arg nativeOnly true --run 'cargo test --locked -p hoki-overlay'
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf -p hoki-overlay'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh ../target/armv7-unknown-linux-gnueabihf/release/hoki-overlay'
nix-shell --arg nativeOnly true --run 'cargo run --locked -p hoki-overlay -- --width 3 --preview /tmp/ring.png'
nix-shell --arg nativeOnly true --run 'cargo run --locked -p hoki-overlay -- --width 3 --preview-alert --preview /tmp/alert.png'
nix-shell --arg nativeOnly true --run 'cargo run --locked -p hoki-overlay -- --width 3 --preview-timers --preview /tmp/timers.png'
```

## Role configuration

Install at `/usr/lib/hoki-overlay`, then send
`set-overlay /usr/lib/hoki-overlay --width 3` to the ceres compositor's
`$XDG_RUNTIME_DIR/hoki-compositor.sock`. `get-overlay` returns its command;
empty `set-overlay` disables it. Configuration persists as `overlay=` in
`~/.config/hoki/shell.conf`.

The role defaults to disabled. One process owns one Overlay layer surface.
Home does not close it. It is composited only while the normal display is
active; it is not installed into the Sidekick ambient watchface.
