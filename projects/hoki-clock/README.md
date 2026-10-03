# Hoki Clock

Slint watch app with alarms, multiple timers, and one stopwatch with laps.
The `hoki-clockd` service owns timekeeping and state, so pressing Home or closing
the app does not stop an activity. The overlay renders timer progress and a quiet
stopwatch marker, then presents Dismiss / Snooze when an alarm or timer expires.
This is a scoped Clock API, not a general notification server.

## Behaviour

- Up to 32 alarms: local HH:mm, once, weekdays, or daily in the UI. The API also
  accepts a Monday-first seven-bit day mask. Alarms can be edited, disabled, or
  deleted. Snooze lasts five minutes.
- Up to 32 timers, each independently pausable/resumable. API duration is one
  second to seven days; the UI has hours/minutes/seconds controls.
- Stopwatch start/pause/reset and up to 100 cumulative laps. The UI shows the
  latest three laps; the snapshot API returns all laps.
- Timers and stopwatch include suspended time using CLOCK_BOOTTIME. Across a
  reboot they use UTC elapsed time, which requires a correctly set system clock.
- Alarm deadlines use local calendar time. Missing DST times skip that day;
  repeated times fire at most once per local date. An overdue saved deadline
  produces one pending alert, not a queue of missed occurrences.
- Alerts vibrate and request a lit screen for at most two minutes. They remain
  pending until dismissed after that period. This version has no alarm audio.
  Wake/haptic errors are exposed to the UI rather than silently claiming success.

State is atomically replaced and synced at `/var/lib/hoki-clock/state.json`.
Corrupt state stops startup and is preserved for diagnosis. Mutations are saved
before acknowledging them. GUI disconnects do not cancel activities.

## Service and API

The system unit runs as ceres, connects to the ceres session bus, and grants only
CAP_WAKE_ALARM for kernel wake timerfds. CLOCK_REALTIME_ALARM schedules alarms;
CLOCK_BOOTTIME_ALARM schedules timers and snoozes. Startup fails if wake clocks
are unavailable. This wakes from suspend, not from a powered-off watch. On-device expiry, screen wake and haptic calls have been checked. Actual
suspend residency/wake remains unvalidated because the device coordinator reports
its existing sensor profile is not ready.

Session bus name and interface: `org.hoki.Clock1`; path `/org/hoki/Clock1`.
`Command(s JSON) -> s JSON` returns a snapshot. `Changed(s JSON)` publishes state.
The shared client lives at `../shared/clock_client.rs`. Examples:

```sh
hoki-clock --command '{"op":"timer-add","seconds":300,"label":"Tea"}'
hoki-clock --command '{"op":"alarm-add","hour":7,"minute":30,"days":31}'
hoki-clock --command '{"op":"stopwatch-start"}'
hoki-clock --command '{"op":"snapshot"}'
```

Other operations: timer-pause/resume/delete/dismiss, alarm-update/toggle/delete/
dismiss/snooze (all take `id`); stopwatch-pause/reset/lap. Alarm update takes the
same fields as add and re-enables the edited alarm. Labels are optional.

## Development

Run from this directory:

```sh
nix-shell --arg nativeOnly true --run 'cargo test --locked -p hoki-clock -p hoki-overlay'
nix-shell --arg nativeOnly true --run 'cargo build --locked -p hoki-clock'
nix-shell --arg nativeOnly true --run 'dbus-run-session -- python3 tests/daemon.py'
nix-shell --arg nativeOnly true --run 'cargo run --locked --bin hoki-clock -- --preview /tmp/clock.png alarm'
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf -p hoki-clock -p hoki-overlay'
```

Preview pages: alarm, timer, stopwatch, edit. The integration test uses a private
bus, temporary state, and explicit `--test-no-wake` mode: no haptics, power
inhibitors, or wake capability. Never deploy that test flag.

For standalone installation, patch each ELF with `../../patch-watch-elf.sh`,
install both binaries under `/usr/lib`, the launcher/desktop file from `deploy/`,
and `hoki-clockd.service` under `/usr/lib/systemd/system`; enable the system unit.
Install the matching overlay to obtain ring activities and full-screen alerts.
Image and runtime-bundle packaging include these files. Deployment requires
separate authorization under the layer's CLAUDE.md.
