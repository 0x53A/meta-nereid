# Everyday sleep

The opt-in sleep coordinator is separate from the existing core-count leases.
It owns everyday suspend; compositor/HWC owns display transitions and the health
recorder owns collection, readiness and durable maintenance. CPU utilization is
not permission to suspend. A background application must hold an inhibitor.

Settings exposes automatic system sleep, sensor profile and the current blocking
reason. Defaults are disabled, sensors off and at most 15 seconds per sleep.
Power configuration is atomically persisted in `/var/lib/hoki-powerd/sleep.json`.
Watchface mode, ambient face and idle timeout belong to the compositor, in
`~/.config/hoki/display.json`; Settings updates them through its control socket. Collection profiles are independent of the
automatic-sleep switch: selecting a profile explicitly starts recording.

## Display and registration

The existing compositor primary watchface role and `set-watchface` control remain
the registration mechanism for full Wayland apps. Ambient providers install a
version-1 JSON manifest in `/usr/share/hoki/ambient-faces/`; see the LP watchface
README. They use a trusted host renderer, not executable Sidekick firmware.

`primary` retains the normal face and blanks after idle; `secondary` uses the
ambient face whenever the watchface role is selected; `automatic` switches the
watchface after idle. All modes allow interactive apps. Foreground apps prevent
automatic sleep; explicit screen-off can sleep them unless they hold an inhibitor.
A wake from secondary-only opens the launcher. Apps and Wayland surfaces survive
screen-off and ambient transitions. The compositor does not issue frame callbacks
while the screen is off. Maintenance wakes retain the ambient face.

The proxy prepares bounded resources, drains its frame fence, enters HWC
DOZE_SUSPEND, then starts Sidekick display. Exit releases Sidekick before HWC ON.
An uncertainty marker survives proxy restart; recovery runs before HWC creation.
Helper calls have deadlines. Failed handoff attempts restoration and latches an
error until user wake/config change; failed restoration terminates the owner.
Actual firmware recovery and local-time rendering require device validation.

## Linux inhibitors and application contract

Use systemd-logind's standard
[`Inhibit`](https://github.com/systemd/systemd/blob/main/docs/INHIBITOR_LOCKS.md)
API for background work. The returned FD must stay open for the operation's
lifetime. `sleep` block locks prevent sleep; delay locks are negotiated by logind
through `PrepareForSleep`. Powerd also treats `idle` block locks as preventing its
automatic suspend. Core-count leases do **not** inhibit sleep.

Powerd calls `SuspendWithFlags(ROOT_CHECK_INHIBITORS)` and waits for both prepare
and resume signals. There is no raw `/sys/power/state` fallback. Missing logind or
an unsupported method blocks sleep. The installed systemd-suspend ExecStartPre
gate rechecks readiness, inhibitors, wake deadline and kernel `wakeup_count`.
Uncoordinated logind suspend requests are rejected by this gate, including when
automatic sleep is disabled. Bounded research tools remain explicitly separate.

Watch applications additionally use `projects/shared/sleep_client.rs` for
CPU/display inhibition. A connection owns its requests; disconnect releases only
that owner's locks. Acquisition during preparation cancels that transaction and
returns a retry error; clients must receive success **before** starting work.
The compositor acquires a CPU inhibitor before restoring interactive display or
launching foreground work, and releases it only after physical ambient/off
handoff and acknowledgement of that exact readiness revision. An in-flight sleep can delay this wake until the grant succeeds.
Audiobook playback holds both this CPU inhibitor and a logind sleep FD, permits
ambient display, and releases them after pause/stop/EOS/error. Loss of its
coordinator connection stops playback instead of continuing unprotected.
Music and Podcasts also hold watch CPU inhibitors during playback, release on
pause/stop/end/error, and stop output if their coordinator connection is lost.
Other background clients must adopt an inhibitor before starting protected work;
powerd cannot infer this safely from CPU load or a process name.

The version-1 newline JSON socket is `/run/hoki-powerd/control.sock`, restricted
by peer credentials to root and ceres. Commands: `status`, `configure` (complete
config), `configure-patch` (atomic changed fields), `inhibit` (cpu/display/reason), `ui`, `sensor`, `sensor-closed`,
`sensor-recovered`, root-only `sensor-idle`, and root-only `commit-sleep`. The compositor is a single UI
owner; sensor registration/recovery is root-only. Status includes configuration,
generation, actual reported UI readiness, reason, deadline, inhibitors and last
suspend result. There is no display target. A UI report contains `display`,
`ready` and a monotonic `revision`; only completed noninteractive states may
report ready. Revisions belong to the UI socket connection. Old revisions or
contradictory repeated revisions are rejected; non-ready reports cancel pending
suspend. Missing or stale UI owners block sleep.
Inputs are bounded; malformed, stale or missing owners fail closed.
Use `configure-patch` with `patch: {"max_sleep_seconds": 30}` for single-field edits;
merging, validation and persistence happen under the coordinator lock. Nested
`auto_cores` patches preserve other core settings. `configure` intentionally
remains a complete replacement for provisioning tools. Deploy compositor, powerd and Settings together after splitting existing display
fields out of sleep.json. This is a coordinated replacement, with no runtime
compatibility layer.

## Sensors

The boot-enabled health policy service defaults to off. It starts only its own
`hoki-health-profile-recording.service`; the manual recording service remains
separate. Both use exclusive setup ownership and existing archive/storage/battery
limits. Starting/cleaning a capture currently restarts sensorfw. Captures are not
restarted automatically after failures, battery stops or full storage: change the
profile (off then on) to make a fresh attempt. Archives are never deleted here.

After clean finalization and sensorfw restoration, the policy may report
`sensor-idle` for the selected profile. This root-only, connection-owned
acknowledgement allows sleep without pretending the selected recording is active.
It expires after 30 seconds, is tied to the current configuration generation,
and cannot override a registered sensor's maintenance requirements, cleanup
faults, or an installed recording override. Policy renews it while the unit
remains inactive; startup acquires an inhibitor before revoking it. Interrupted
or failed finalization still blocks sleep. Per-service `profile-latest.json`
and `manual-latest.json` preserve stop state after systemd removes the runtime
directory; previous-boot records are not accepted. Deploy powerd before the
updated health policy and recording-session scripts. Host regressions and live
active-owner rejection/clean-stop acceptance passed on 2026-10-06. A battery
control with Full still selected and recording cleanly stopped completed 16
suspends in 90 seconds. Failed/interrupted cleanup was not bypassed.

| Profile | Requested Android sensor types |
|---|---|
| daily | step detector (18), step counter (19), heart rate (21) |
| sleep | daily plus accelerometer (1) |
| activity | sleep plus gyroscope (4) |
| full | all channels supported by the existing selector |

Unavailable channels are omitted; an empty profile is rejected. These are capture
presets, not health diagnoses. Daily/sleep use descriptor-clamped 1 s/200 ms
periods; activity/full retain descriptor-derived rates. Ordinary captures request
zero HAL latency. Only plans consisting wholly of wakeup streams may release their
CPU inhibitor. Non-wakeup plans keep Linux awake.

The recorder arms its fallback alarm before reporting readiness, drains and
checkpoints, and holds a CPU inhibitor during maintenance. Powerd uses the
earliest recorder deadline and its own bounded alarm. Disconnected sensor owners
latch a fault until owned cleanup succeeds. The research suspend worker refuses
to compete with enabled everyday coordination.

The manual service has a separate bounded full-profile probe, enabled only by an
explicit `HOKI_BUFFERED_FULL_TRIAL=1` service-manager environment. It requests
period-aligned 7, 20 or 40 second HAL latency for every continuous wakeup
descriptor and uses a fallback flush/deadline ten seconds later (17, 30 or 50
seconds). The default step is 7 seconds; duration defaults to 300 seconds and is
capped at 1800 seconds. On-change, one-shot and special-reporting streams remain
immediate. FIFO metadata is retained per descriptor to classify whether a step is
within its advertised reservation, exceeds it, has zero reserved entries, or is
unknown. The probe deliberately does not clamp requests to that metadata: event
continuity and loss are experimental outcomes, not assumed guarantees. Task0201's
7-second test covered 16 wakeup streams and showed continuous gyro and several
other streams; the current full profile selects 29, so that earlier result does
not establish safety for the expanded plan. Ordinary Settings/manual defaults do
not enable this probe.

## Validation limits

Automatic sleep is off by default. USB/charger state must be known safe;
unknown Wi-Fi interface state blocks entry. Enabled Wi-Fi permits suspend.
The systemd-suspend unit runs `hoki-wifi-sleep prepare` before the final gate:
it enables WoWLAN ANY only when no triggers are configured, then issues Prima's
Android `SETSUSPENDMODE 1`. This preserves association through cfg80211 suspend;
ANY does not establish selective firmware wake filtering. Existing triggers are
left intact. `ExecStopPost` resumes the driver and restores disabled WoWLAN on
both resume and failed entry. A retained `/run/hoki-wifi-sleep/state.json`
requires successful cleanup before another attempt. Deploy the daemon, helper,
and suspend unit drop-in together; Python fcntl and iw are required.
Hoki's alarmtimer rejects deadlines less than two seconds away. Sensor readiness
now reserves one second for preparation beyond the final-entry margin; the
normal final gate requires three seconds remaining, including headroom for
sync/freezing. The legacy three-second maximum retains a two-second final
margin. This avoids beginning routine preparation at the kernel's exact limit;
it does not eliminate races with unrelated alarms or fresh wake events.
Failed transactions and attempts with no measurable residency back off
exponentially. A completed transaction with at least 5 ms of residency resets
that failure count; ordinary early radio/sensor wakes below 0.5 s use a fixed
two-second cooldown, longer sleeps one second. Short successful sleeps must not
grow into minutes of awake time. Suspend
residency is BOOTTIME minus awake elapsed time, not merely a successful D-Bus call.

On the watch, validate repeated handoffs/app retention, crown/touch wake, all face
variants and local time, proxy/compositor recovery, audiobook playback, sensor
archive continuity and overflow, standard block/delay locks, charging transitions,
radio-specific suspend behavior, and long-run residency/energy before enabling
by default. Current host tests cover policy, IPC ownership, a private logind bus,
app retention, descriptor layouts, profiles and service ownership; they cannot
establish Sidekick visuals, sensor FIFO safety or battery improvement.

Connected suspend was deployed and tested on hoki on 2026-10-03 with both radios
enabled. A buffered Full window retained Wi-Fi association and recording across
five deep sleeps and three s2idle fallbacks (3.60 s suspended / 93.87 s elapsed);
three deep attempts aborted on the recorder wake source. A recorder-off control
completed 18 deep sleeps (21.80 s / 97.00 s), with four WLAN callback failures.
These short windows establish connected entry/resume, not efficient standby.
An inhibited, uncoordinated systemd suspend request was rejected by the final
gate, with unchanged kernel counters, driver cleanup, WoWLAN disabled again, and
no remaining preparation marker. Full recording was restored afterward.
