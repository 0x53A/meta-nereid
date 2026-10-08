# Hoki power daemon

Provides `org.hoki.power.Manager` at `/org/hoki/power` on the system bus.
CPU leases request 1–3 extra cores beyond CPU0, for at most 300 seconds.
Combined requests are capped at four online cores; external power keeps all
four online. Releasing a lease returns its demand early.

Successful reconciliation also selects the CPU0 cpufreq `ondemand` governor
and writes `N` to the low-power idle `sleep_disabled` parameter. These are
maintained policy settings, not just boot-time underclock setup. A failed sysfs
write aborts that reconciliation and is reported; source policy alone does not
prove the running device accepted it or establish frequency/idle residency.

Lease methods return a string: a lease ID or `error: ...` for requests, and `ok`
or `error: ...` for releases. If applying a new request fails, its lease is
removed and the daemon attempts to reapply the remaining policy. Release removes
the lease before applying settings, so an application error does not restore
that lease; retrying the same release returns `error: unknown lease`. Later
periodic reconciliation retries the remaining policy. Sysfs updates can partially
succeed before an error, so neither failure path guarantees immediate hardware
rollback.

Reconciliation currently runs on a single-thread Tokio runtime. The pinned zbus
Tokio executor dispatches methods on that runtime too. After obtaining the lease
snapshot, reconciliation performs its synchronous sysfs writes without another
await, so another daemon task cannot apply a newer snapshot in the middle of
those writes. Changing to a multithread runtime, adding an await in that interval,
or offloading writes requires reviewing this ordering assumption. This does not
make the individual sysfs writes atomic or coordinate external sysfs writers.

Lease deadlines use Linux `CLOCK_BOOTTIME`: time asleep counts, and wall-clock
corrections do not extend or shorten a lease. Leases do not arm wake alarms.
Expired demand is removed at the next regular reconciliation after resume;
the daemon normally reconciles every five awake seconds. Status excludes expired
leases even before that sweep. A clock-read failure is reported rather than
silently substituting another clock domain.

Reconciliation and battery logging skip missed timer ticks after an executor
delay. They take one current observation when work resumes, then return to their
normal schedule, instead of replaying past deadlines as back-to-back sysfs reads.
The immediate first tick is preserved. This does not add a system wake alarm.

Battery log `wifi-up`/`wifi-down` events describe the WLAN interface's administrative
`IFF_UP` flag, not association or Internet access. Invalid reads do not invent
state transitions. CPU and charger policy are separate from these log events.

Charger transition logs likewise skip unknown observations: failed directory
reads, malformed `online` values, or unreadable supplies do not manufacture a
disconnect event. A known online supply establishes connection even when another
cannot be read. Supplies without an `online` attribute are ignored. The existing
CPU-policy and boolean status fallback still treats an unknown observation as
not connected; no new fallback policy is introduced by the logging check.

The opt-in [everyday sleep coordinator](SLEEP.md) now manages automatic suspend,
owner-bound inhibitors and sensor deadlines. The compositor/proxy owns display
power and Sidekick handoff. A dark or ambient display alone does not establish
suspend residency. Core leases above remain independent of suspend inhibitors.

Battery entries sample sysfs attributes sequentially. At charger transitions,
status, current, percentage, and charge counter can describe different instants;
the fuel gauge can also adjust its estimate. For discharge averages, use a
contiguous discharging interval, inspect counter continuity, and exclude the
charger boundary. The logged current is an instantaneous sample, whereas charge
counter differences estimate consumption over the interval.
Unavailable current and voltage reads print `?`; unavailable capacity and charge
counter reads use `-1`. These markers must not be treated as measurements.

Host validation: `cargo test` from this directory. Tests do not write power or
CPU sysfs files or contact the system bus. The logind integration test starts
an isolated private `dbus-daemon`.

Build from this directory:

```sh
nix-shell --run "cargo build --release --target armv7-unknown-linux-gnueabihf"
nix-shell -p patchelf --run "bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-powerd"
```

Deployment and service restart are separate operations; see the repository's
`CLAUDE.md` service workflow.

## Automatic CPU cores

Settings exposes one **Automatic CPU cores** toggle, default off. This policy is
independent of automatic system sleep. Explicit core leases establish a minimum;
charging still requests all four cores. Automatic demand adds at most one core
per decision and is capped separately by `max_cores` (leases can exceed that cap).
It never acquires a suspend inhibitor or arms a wake alarm.

Read or change the running policy over SSH, as root or ceres:

```sh
/usr/local/bin/hoki-powerd --auto-cores
/usr/local/bin/hoki-powerd --auto-cores '{"enabled":true}'
/usr/local/bin/hoki-powerd --auto-cores '{"up_percent":90,"up_seconds":3,"down_seconds":15}'
/usr/local/bin/hoki-powerd --auto-cores '{"enabled":false}'
```

Updates are atomic partial patches, validated before persistence and applied
without restart (next reconciliation, normally within five awake seconds;
failed-write backoff can delay hardware changes further). Unspecified fields are preserved;
unknown keys and invalid combinations are rejected. The Settings toggle uses the
same patch endpoint. Values persist in the `auto_cores` object of
`/var/lib/hoki-powerd/sleep.json`; older files without that object load defaults.
Use the command rather than editing the file: live file reload is not provided.

| Parameter | Default | Meaning |
|---|---:|---|
| `enabled` | false | Allow heuristic core demand |
| `sample_ms` | 500 | Non-waking sample interval (100–5000 ms) |
| `up_percent` | 85 | Minimum online-CPU utilization for scale-up |
| `up_seconds` | 2 | Full scale-up observation window (1–60 s) |
| `sustained_fraction` | 0.75 | Fraction of the window satisfying both conditions (0.5–1) |
| `down_percent` | 60 | Maximum utilization projected onto one fewer core |
| `down_seconds` | 10 | Full scale-down window (2–300 s, at least up window) |
| `dwell_seconds` | 2 | Minimum time between decisions (0.5–300 s) |
| `max_cores` | 4 | Maximum automatic total cores (1–4) |

Down threshold must be below up threshold, and the up window must contain at
least two sampling intervals. Percentages are integers; time windows can be
fractional seconds. Disabling or retuning clears prior heuristic demand/history.

The sampler uses checked per-online-CPU deltas from `/proc/stat`, excludes
idle/I/O-wait/steal and avoids double-counting guest time. Contention uses sampled
`procs_running` minus the sampling thread, not all process threads or load average.
Scale-up requires more runnable work than online CPUs; scale-down also requires
runnable demand to fit on the smaller set. These instantaneous runqueue samples
are deliberately conservative estimates, not exact measurements of parallelism.
See the [kernel proc documentation](https://kernel.org/doc/html/v6.15/filesystems/proc.html).

Windows reset on hotplug, configuration changes, counter regression, suspend
(BOOTTIME versus monotonic), and missed samples. Suspend/gaps/read errors discard
automatic demand; charging starts fresh observation afterward. Failed writes
clear heuristic demand and back off before retry. Existing lease/charging demand
remains authoritative. Status reports the automatic target and observation/reason;
core changes and the supporting load/runnable observations appear in the journal.
Hardware responsiveness and energy tuning remain unverified until watch testing.

## Display brightness

Settings → Brightness offers a 1–100% normal-mode manual slider and separate
Auto · Normal mode and Auto · Low-power face checkboxes. Preferences live in the `brightness` object of
`/var/lib/hoki-powerd/sleep.json`; old configurations default to 50%, manual.
The control socket accepts `configure-brightness` with a partial `patch`
(`level`, `automatic`, and/or `ambient_automatic`). Old configurations preserve
their normal-mode preference and default low-power automatic brightness to off.
This changes preferences without changing sleep
policy or its generation. Invalid levels and unknown fields are rejected.

The HWC proxy applies these preferences to the interactive panel within roughly
one second. It serializes writes with display ownership transitions and releases
its light-sensor session before screen-off or Sidekick handoff. Its worker then
waits on display-state notification, with no periodic powerd queries or status
file writes until normal display ownership resumes. An already-running powerd
query may finish during the transition; its result cannot apply to an off display.
No MCE is involved.

Low-power auto-brightness is applied on the next Sidekick entry, through native
ALS mode ON (2), without Linux ALS polling. The initial five-band curve uses
the face bundle's brightness/dim levels as ceilings and hysteretic lux thresholds.
With it off, the existing bundle levels and ALS OFF sequence are preserved.
Managed exit disables native ALS before releasing the display, including before
screen-off. The new native automatic path and curve require physical validation;
compilation and transport-order tests do not establish firmware acceptance or
power savings. Update powerd, Settings, HWC proxy and LP renderer together.

Automatic brightness uses sensorfw's ALS session, a logarithmic 10–100% curve
and a 3 percentage-point deadband. Missing light data falls back to the saved
manual level, with a visible notice in Settings and retries every ten seconds.
The proxy reports application/sensor status in
`/run/hoki-hwc-proxy/brightness.json`; Settings rejects stale status. Sensorfw
may be unavailable while another capture owns the sensor HAL. Automatic mode
never starts or restarts sensorfwd and does not acquire a suspend inhibitor.
Physical response, sensor behavior and curve comfort require watch validation.
