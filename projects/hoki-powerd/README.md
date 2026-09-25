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

The daemon does not implement display blanking or automatic system suspend.
The compositor owns display power; its `display-on`/`display-off` notifications
only add battery log entries here. A dark display therefore does not establish
that the system slept. The health recorder's session-owned suspend worker is a
separate mechanism and is not an ordinary idle policy.

Battery entries sample sysfs attributes sequentially. At charger transitions,
status, current, percentage, and charge counter can describe different instants;
the fuel gauge can also adjust its estimate. For discharge averages, use a
contiguous discharging interval, inspect counter continuity, and exclude the
charger boundary. The logged current is an instantaneous sample, whereas charge
counter differences estimate consumption over the interval.
Unavailable current and voltage reads print `?`; unavailable capacity and charge
counter reads use `-1`. These markers must not be treated as measurements.

Host validation: `cargo test` from this directory. Tests do not write power or
CPU sysfs files or contact the system bus.

Build from this directory:

```sh
nix-shell --run "cargo build --release --target armv7-unknown-linux-gnueabihf"
nix-shell -p patchelf --run "bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-powerd"
```

Deployment and service restart are separate operations; see the repository's
`CLAUDE.md` service workflow.
