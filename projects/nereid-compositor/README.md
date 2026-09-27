# nereid-compositor

Minimal Rust Wayland compositor for AsteroidOS, targeting the Fossil Gen 6 (hoki).
Renders via libhybris hwcomposer EGL backend, replaces the Qt/lipstick compositor stack.

Hyprland-style: the compositor only composites surfaces and routes input.
Watchface, launcher, notifications, etc. are separate Wayland client apps.

## App launch CPU lease

Before spawning an app requested by the launcher, the compositor asks
`hoki-powerd` for one extra CPU core for three seconds. A typed zbus proxy makes
the request on a worker thread and sends the launch back to the compositor event
loop after the reply. The method reply timeout is 150 ms; a missing or rejecting
power daemon does not prevent app launch. Charging already keeps all cores
online. Frequency remains under powerd's `ondemand` governor. Launch latency
and energy impact still need measurement on the watch.

## Output capture

The compositor advertises `ext_output_image_capture_source_manager_v1` and
`ext_image_copy_capture_manager_v1`, version 1.
Capture clients use the normal local Wayland socket; there is no network server
or VNC dependency. The socket's existing local access policy also governs capture.

Client sequence:

1. Create an output capture source from `wl_output`, then a capture session.
2. Wait for `buffer_size`, `shm_format`, and `done`. Allocate a matching
   `wl_shm` XRGB8888 buffer; padded strides and pool offsets are supported.
3. Create a frame, attach the buffer, damage its full extent, and request capture.
4. On `ready`, consume the buffer, destroy the frame, and create the next frame.
   Encoding/transport should use a buffer ring so they do not delay capture.

The first frame can use the latest composed image even if the screen is static.
Later requests wait for the next presented image, with no capture FPS cap or
timer. The output currently advertises 45 Hz. A slow consumer skips intervening
images; it cannot hold up another session or cause an unbounded frame queue.
Full-frame damage is reported, with normal transform and CLOCK_MONOTONIC time
sampled after HWC's presentation acknowledgement. This is an approximate
presentation timestamp, not a hardware fence timestamp.

Captures include the visible app and composed layers. Normal display-off and
ambient entry stop sessions and fail pending frames; capture does not wake the
watch or inhibit sleep. Create a new session after waking. Sidekick's autonomous
face is not captured. DMA-BUF, individual-window capture and separate cursor
images are not supported; cursor-session requests produce a stopped session.
There is currently no rendered pointer cursor, so `paint_cursors` changes nothing.

Socket-based protocol tests run without the watch via
`nix-shell --run 'cargo test --bin nereid-compositor'`. Sustained frame rate and
power use still need an on-watch measurement.

## Build

Requires rustup with `armv7-unknown-linux-gnueabihf` target installed.

```sh
nix-shell
cargo build --release --target armv7-unknown-linux-gnueabihf
```

## Package

```sh
nix-shell --run ./build-opk.sh
```

Produces `nereid-compositor_0.1.0_armv7vehf-neon.opk` (~470K).
Conflicts with and replaces: `asteroid-launcher`, `lipstick`, `qt5-qpa-hwcomposer-plugin`,
`asteroid-launcher-configs`, `qtscenegraph-adaptation`, `mapplauncherd`,
`mapplauncherd-qt`, `mapplauncherd-booster-qtcomponents`.

## Install

```sh
scp nereid-compositor_0.1.0_armv7vehf-neon.opk root@hoki.local:/tmp/
ssh root@hoki.local 'opkg install --force-conflicts /tmp/nereid-compositor_0.1.0_armv7vehf-neon.opk'
```

## Managed sleep

The compositor now reports interaction, foreground role and acknowledged display
state to [powerd](../hoki-powerd/SLEEP.md). HWC proxy owns the Sidekick transaction.
Apps, surfaces, focus and buffers survive ambient/screen-off; input and frame
rendering remain under compositor control. Coordinator IPC runs outside the UI
thread, stale idle responses are discarded after activity, and the legacy display
timeout is used only when managed sleep is disabled. Assistant mode counts as a
foreground application just like launcher/settings and ordinary app mode.

## Assistant role

`agent=` in `~/.config/hoki/shell.conf` selects an optional managed assistant
command (shell-quoted argv, executed directly). The private compositor control
socket accepts `set-agent <argv>`, empty `set-agent` to clear, and `get-agent`.
A 650 ms crown hold activates it and preserves the previous foreground app.
With a role configured, a short crown action happens on release. A fully dark
display consumes the wake tap. A visible ambient watchface uses the primary
watchface button actions: upper opens Settings, crown opens Launcher, and lower
turns the display off. The assistant receives `activate`, `cancel`, visibility and scroll
messages on stdin and may write `dismiss` on stdout to return to the prior app.
Logging belongs on stderr. Top/bottom buttons forward F13/F14.

The first client is [hoki-argyroneta](../hoki-assistant/README.md). This role uses
a managed toplevel. The existing `zwlr_layer_shell_v1` implementation also allows
alpha-composited Top/Overlay surfaces and checks surface input regions before
routing touches. It currently configures layers to display dimensions, so this
is not a claim of complete layer-shell placement/exclusive-zone support.
