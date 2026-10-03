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
The watch output has no pointer cursor; the virtual desktop cursor is described below.

Socket-based protocol tests run without the watch via
`nix-shell --arg nativeOnly true --run 'cargo test --locked --bin nereid-compositor'`. Sustained frame rate and
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

## Remote display off

Send `screen-off\n` to the existing private Unix control socket
`$XDG_RUNTIME_DIR/hoki-compositor.sock` (normally
`/run/user/1000/hoki-compositor.sock` on the watch). Connect as the compositor
user or root. The reply `ok\n` means the request was queued; the compositor
applies its normal manual screen-off transition on its event loop, preserving
apps and coordinating with powerd. This is not a direct sysfs display toggle.
A disconnected compositor returns `error: compositor unavailable\n`.

For example, from an authenticated watch shell with Python available:

```python
import socket
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
    connection.settimeout(5)
    connection.connect('/run/user/1000/hoki-compositor.sock')
    connection.sendall(b'screen-off\n')
    print(connection.recv(256).decode().strip())
```

Check powerd status if completed display-off acknowledgement is needed; the
socket reply alone does not certify that hardware has powered down. Wake the
watch normally with its button.

## Virtual desktop output

The compositor advertises `zwlr_output_manager_v1` version 1 with two heads:
`hoki-display` (fixed watch configuration) and `hoki-desktop` (initially disabled).
The desktop defaults to 1280×720 at 30 Hz and scale 1. A host can enable it with
standard output-management transactions; no Pict or PipeWire dependency is needed.

Host sequence:

1. Bind the manager, collect both heads, and retain its latest `done` serial.
2. Create a configuration using that serial. Enable the watch head without changing
   its properties. Enable the desktop head and set its custom mode (pixel width,
   pixel height, refresh in **mHz**) and scale. Every transaction must include both
   heads. `test` validates without changing anything; `apply` changes atomically.
3. Wait for `succeeded` and the desktop `wl_output` named `hoki-desktop`; then use
   `ext-output-image-capture-source-v1` and `ext-image-copy-capture-v1` on it.
4. Request frames only when the encoder/transport can accept another frame.
   Composition and visible desktop frame callbacks run only with capture demand,
   at no more than the configured refresh rate. There is no queued frame backlog.
5. Resize/scale/refresh changes stop old capture sessions and fail pending frames.
   Destroy/recreate the source and session, then allocate buffers using the new
   constraints. A stale configuration serial receives `cancelled`; refresh it and
   retry. Disable the desktop head to remove its `wl_output` and stop capture.

Compositor resource limits are 64–4096 pixels per axis, at most 8,388,608 pixels
(32 MiB RGBA), 1–120 Hz, and integer scale 1–4 with divisible dimensions. Refresh 0
selects 30 Hz. Only normal transform and position (0,0) are supported because these
are separate desktops, not an extended coordinate space. Unsupported combinations
receive `failed`. The host must additionally enforce encoder/receiver limits;
these compositor bounds are not a performance guarantee.

Launch desktop app **binaries** with `WAYLAND_DISPLAY=wayland-desktop`; normal
`wayland-0` clients remain watch apps. Existing watch launcher scripts overwrite
this environment and must not be used for desktop launch. Desktop xdg toplevels
are fullscreen with independent focus and integer-scaled logical configuration.
The last mapped desktop app is foreground; unmapping restores the previous one.
Apps remain assigned to the desktop while it is disabled. Desktop layer surfaces
are closed; popups/subsurfaces and general window management remain unsupported.
Both outputs/seats are discoverable through the trusted local sockets; routing
and focus are separated, but this is not a security boundary or registry isolation.

The `desktop` seat receives USB/Bluetooth keyboard, relative mouse motion,
buttons and wheel input, classified through udev bus metadata. Watch touch,
buttons and crown retain their existing path. Desktop input never wakes the panel
or updates watch idle time. Disabling releases held keys/buttons. A simple white
compositor cursor is included only when capture requests `paint_cursors`; separate
cursor capture and client cursor images are not yet supported. Remote virtual
keyboard/pointer protocol support belongs to the upcoming host integration.

Desktop capture continues when the panel is off, provided the system stays awake.
The future host must hold the CPU/suspend inhibitor; the compositor does not acquire
one for capture. Disconnecting a manager does not disable the desktop (the protocol
provides no ownership lease). Losing capture demand stops rendering, while apps
remain mapped for reconnect. Pict/Venus hosting, pairing UI and a rectangular
launcher are subsequent work.

## Persistent overlay role

`overlay=` in `~/.config/hoki/shell.conf` selects one optional managed process
(default: disabled). `set-overlay <argv>`, `get-overlay`, and empty `set-overlay`
on the private control socket configure it persistently. It starts with the
compositor and restarts with the existing two-second crash backoff regardless
of foreground mode, including while the display is off. It receives visibility
messages and is stopped on replacement or compositor shutdown.

This role uses one `zwlr_layer_shell_v1` Overlay surface, matched by client PID.
Additional layer surfaces, non-Overlay layers, and xdg toplevels from that role
are closed. It is independent of foreground shell modes and survives Home.
Other clients' layer-shell support is unchanged. Apps still receive the full
display size; no exclusive-zone or ring inset has been added.

[hoki-overlay](../hoki-overlay/README.md) is the initial Slint ring client. It
uses an empty input region and no keyboard interactivity, so touches reach the
underlying app. The current ring is static and has no status/notification data.
Notification-aware watchface capabilities and complication providers are future
work. Normal display-off/ambient retains the process without drawing the ring
on the autonomous Sidekick watchface.

## Authentication lock role

`lock-screen=` in `~/.config/hoki/shell.conf` selects a replaceable standalone
renderer, initially `/usr/lib/hoki-lockscreen`. The role is opt-in; no configured
command preserves existing startup behavior. Once configured, startup is locked,
and the display stays blank until the root-owned system-bus `io.Nereid.Auth1`
service reports its initial state. A verified state with no enrolled PIN and
`locked=false` unlocks immediately without starting the renderer; a configured
PIN remains subject to Auth1's lock state. The renderer's stdout cannot unlock
or navigate the shell. `get-lock-screen` and
non-empty `set-lock-screen <argv>` are supported by the control socket; removing
lock configuration requires an explicit config change and compositor restart.

The renderer is a managed xdg toplevel identified by its launched child PID,
not an app-id. While locked, Home (the crown press) toggles between the PIN
renderer and a display-only watchface. The lower pusher turns the display off;
a press on a dark display only wakes it. Tapping the locked watchface returns
to PIN entry and consumes that touch. The upper pusher retains the PIN
renderer's editing action only when PIN entry is visible.

This is a view selection inside the lock, not an authentication transition.
Only the selected surface is composed over black. Generic layers, apps,
overlays, launches and both output capture paths remain suppressed. Missing
selected content leaves black rather than falling back to an app. Returning
to the watchface or blanking the display never unlocks encrypted storage.
Renderer visibility uses the existing stdin role protocol. Any renderer may
implement this role using the shared [encrypted PIN client](../nereid-auth/README.md).

Watchface processes receive `lock-state:locked` or `lock-state:unlocked` on
stdin initially and when the compositor's effective state changes, before
visibility publication. This applies to the primary and companion watchfaces.
A watchface may write and flush `get-lock-state` on stdout to request the
current value, including while locked. The reply is informational: watchface
output cannot change authentication state. A renderer may use the value to
change its appearance; no particular locked style is required.

Clients can also query the underlying auth service directly on the system bus:
`io.Nereid.Auth1.GetState` at `/io/Nereid/Auth1` returns enrolled/locked/busy,
and its `Locked` property emits changes. Treat service loss or a failed read as
locked. The compositor's role notification additionally reflects its configured
lock-role policy and startup/owner-change handling.

The auth monitor polls every 250 ms, with bounded bus calls; it resolves and
rechecks the service's unique owner and reads enrollment and lock state together.
Missing service, read failure or owner loss locks first. A changed owner also
locks first when a PIN is enrolled, until a second stable sample arrives. Owner-loss
detection is therefore bounded polling, not instantaneous.
This is an initial screen lock, not a claim of secure boot, encrypted storage,
or isolation from malicious applications running as the same user.
