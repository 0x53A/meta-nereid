# Session supervision

Build the installable tool payload from repository root with
`bash meta-nereid/build-health-recorder.sh`. It builds the Rust controller in
its own cross environment, patches the glibc interpreter/RPATH, builds the Android
SSC helper, and archives both plus the suspend coordinator and documentation.
The package installs `/usr/bin/hoki-health-recorder`,
`/usr/libexec/hoki-ssc-recorder` and `/usr/libexec/hoki-recording-suspend-loop`.
Binary checksums and a source fingerprint are installed under
`/usr/share/hoki-health-recorder`.

Custom Hoki image builds include this package and reject a missing/stale payload
before syncing to the builder. The SSC helper is a separate bionic subpackage;
only its vendor ABI dependency check is excluded from glibc package scanning.
The existing sensorfw recording patch is included by the local layer. No recorder
boot service, sensor activation or suspend policy is enabled by installation.
Session provisioning and independent recovery remain explicit requirements.
Task0308 tracks package QA and deployment separately from local payload validation.

The collector processes need a shared session lifetime. Tasks0262–0263 exercise the
systemd relationship before packaging a permanent service:

- A session target has BindsTo and After dependencies on both HAL and SSC services.
- Both services have PartOf pointing to that target.
- Stopping the target stops both services; losing either service stops the target
  and therefore its peer. No automatic restart hides a partial recording.
- HAL ExecStopPost runs ownership-checked cleanup against its saved session.
- SSC ExecStopPost releases only its own abandoned wake hold under its file lease.
- Service stop deadlines bound vendor/filesystem hangs; captures then retain
  incomplete status and require recovery assessment.

For a bounded trial with less available storage, `session_units.py
--hal-limit-bytes BYTES` accepts 128 MiB through 1 GiB (default 1 GiB). It passes
`HOKI_HAL_LIMIT_BYTES` to both admission and HAL recording and saves the value in
the manifest. This changes only the file-size budget, not sensor selection or
rates; reaching it still fails the session. For 512 MiB HAL, combined admission
requires 912 MiB (933888 KiB) with current SSC/reserve/headroom settings. The
controller records the actual budget. A smaller budget is not proof a requested
recording duration will fit; verify the resulting capture and rate.

Before applying sensor configuration, check available space on the capture
filesystem against **both** archive budgets. Current combined trials require
1458176 KiB (1424 MiB): HAL 1024 MiB, SSC 128 MiB, one shared 256 MiB free-space
reserve, and 16 MiB for metadata/startup. Repeat this check immediately before
installation/activation, after copying trial binaries. The HAL constructor itself
requires its entire 1 GiB budget plus reserve and 64 KiB headroom; the SSC journal
checks reserve incrementally. A 1 GiB preparation check is insufficient. These
checks are admission estimates, not reservations against concurrent writers;
ongoing write failures still stop the session and preserve incomplete evidence.
The generated profile-activation drop-in now enforces this check through
`ExecStartPre=... --check-recording-space RECORDING_ROOT RECORDING_ROOT/ssc`,
before profile setters execute. Both directories must exist, be private and
root-owned, and use the same filesystem. Deploy the matching controller; older
controllers cannot execute this command. A failed check fails activation and
leaves normal supervised recovery in place. Launch scripts should also preflight
before backend installation; task0310 scripts contain that earlier check.

Services must use a fresh private capture directory. Boot identity, discovered
SUIDs and recorded descriptors must belong to the current device/session. A
shared manifest still needs to record child identities and completion outcomes;
a target returning inactive is not proof that both archives completed.

Tasks0262–0263 contain temporary unit files for injected SSC and HAL crashes.
Both failure directions were verified on-watch: the target stopped the peer,
HAL data finalized or recovered, and SSC finalized when it was the surviving
peer. A killed SSC retains an incomplete archive claim. Original failed service
results are preserved even when cleanup succeeds. They use
trial paths and fixed same-boot SUIDs and are not production installation units.
Production setup must arrange sensorfw recording configuration, plugin readiness,
automated discovery, fresh-directory allocation, diagnostics preservation,
retention and suspend coordination. Backend recording currently permits one
capture per sensorfw lifetime, so session rollover also needs explicit policy.

Task0264 also verifies normal target stop with both archives finalized. The HAL
service can now use Type=notify/NotifyAccess=main: READY follows successful sensor
activation, backend health check and saved activation metadata. Set a startup
timeout. Task0265 adds SSC Type=notify readiness after the first completed minute transfer
and durable archive snapshot (currently checked at30s; use a bounded70s startup
timeout). The shared target was observed waiting for both notifications. Neither
readiness notification establishes fresh samples or all health-stream coverage.

Task0267 runs fresh discovery to successful exit, validates its current-boot
inventory with --select-ssc, and supplies SSC_MINUTE_SUID through a private
EnvironmentFile. The capture's recorded endpoint was verified against discovery.
Permanent restoration must explicitly wait for both component service stop jobs
before changing/restarting sensorfw; an inactive target alone is insufficient.

Task0277 tests SSC helper ordering under supervisor SIGKILL. `PartOf` alone did
not immediately stop the child after main-process death. Live backend startup
therefore explicitly quiesces its recorded helpers before restoration: validate
launch records/boot/owner, refuse active supervisors, stop children and confirm
terminal state with no main/control PID. Every helper persists its unit identity
before launch; `Requisite` prevents late child starts against a stopped supervisor.
The read-only crash test verified active-owner refusal, prompt child drain and
late-start refusal. Recovery attempts must avoid reusing an active supervisor name
found in historical launch records; the final service factory is still pending.

Task0278 adds a native bounded configuration-trial factory. It saves runtime and
initial transaction metadata, creates activation/recovery units, runtime-links and
checks them, then starts activation. Activation ExecStopPost queues recovery without
blocking; recovery orders After the stopped activation unit and uses a distinct
attempt UUID. The actual service MainPID is checked before either role runs.
This integrates configuration cleanup only; shared HAL/SSC recording, power policy,
restart/retention and cross-boot reconciliation are still separate work.

Task0279 validates the complete configuration lifecycle under activation SIGKILL,
both after full activation and while the first setter is in flight with only its
intent committed. The independent recovery service restored exact baselines and
released ownership, preserving the original signal9 failures. In the partial case,
readback proved the uncertain write had applied; the outstanding helper was stopped
before restoration. Its controller-side supervisor result is absent due to the kill,
while the preserved systemd journal confirms archive finalization/termination.
These are same-boot tests; shared recording/suspend integration remains separate.

Task0280 combines configuration and recording. A oneshot --await-sleep gate waits
for a durable-active checkpoint plus a PID/invocation-qualified readiness marker;
HAL/SSC units require and order after it. Keeping config Type=exec avoids a startup
deadlock with helper Requisite/After dependencies. The recording target binds to
config lifetime, and recovery orders after the recorder units. The normal script
explicitly waits for both recorders to stop before config stop/restoration, then
reverts the temporary sensorfw override. This ordering was verified live. The trial
still uses scripts and an independent fallback timer; it does not establish full
bidirectional failure coupling or a permanent suspend/retention coordinator.

`suspend-loop.sh CONTROLLER SOCKET CAPTURE HAL_UNIT SSC_UNIT` is the recurring
power worker for a session. Run it as root in a `Type=exec` service named
`hoki-recording-power-UUID.service`, pass that name as HOKI_POWER_SUPERVISOR, and
set BindsTo/After for both recorder units, PartOf for the capture target,
KillMode=control-group and a bounded session runtime. Start it only after both
recorders are ready. The script verifies its MainPID and recorder dependencies.

While charger/USB blocks sleep it defers for30s without spawning suspend attempts.
On battery it launches the bounded --suspend-recording child, whose BindsTo/After
relations include the power worker and both recorders. A successful return waits1s
before the next attempt; failures back off5s then10s, and the third exits explicitly.
The parent must be stopped before configuration restoration. Radio/display policy
and recovery of a failed power worker remain the enclosing session's responsibility.
This worker is a maintained component, not yet permanent boot-time deployment.

`session_units.py RUNTIME_JSON SELECTION_JSON NEW_BUNDLE --recording-root PATH
--socket PATH --power-loop PATH --timezone-seconds OFFSET` stages a private,
exclusive unit bundle. Runtime JSON comes from native --prepare-sleep; selection
JSON contains the same boot_id and endpoints {fsl_min,fsl_sleep,fsl_rhr}, obtained
with native --select-ssc after successful discovery. Preparation compares identities;
it does not itself inspect a live watch or prove that selection provenance is valid.
Normal discovery now includes fsl_rhr and fsl_wk; an ordinary native snapshot
can supply all supported periodic reader endpoints without a separate extended scan.

The bundle contains HAL/SSC/gate/power services, a shared target, and apply/recovery
drop-ins. Recorder/power bounds are profile duration+120s. Profile expiry stops the
whole target; its duration begins before recorder startup, so this is not a promise
of that many seconds after both recorders become ready. Power-worker failure also
stops the whole target. Recovery orders after all workers and the readiness gate.
An inactive target still does not establish archive completeness.

This tool only stages files. Installation/launch must revalidate the current boot,
private fresh output directories, source-qualified discovery, backend compatibility,
and independently loaded profile recovery. It must install both drop-ins before
starting the target and deploy the matching suspend-loop.sh at the configured
power-loop path. Keep profile apply/recovery units linked to their original
prepared files: copying those unit files elsewhere breaks native FragmentPath
identity checks. Backend candidate activation/restoration, UI/radio policy,
retention and cross-boot reconciliation remain separate work. Do not treat the
bundle as a boot-enabled or self-recovering deployment.

Run generator tests with `python3 -m unittest discover -s deploy -p test_session_units.py`.

Run all deployment tests with
`python3 -m unittest discover -s deploy -p 'test_*.py'`.
The suspend-loop tests execute the real shell coordinator with isolated command
substitutes. They cover charger/USB deferral, bounded child service properties and
recorder bindings, failure backoff, stopping after three consecutive failures,
and resetting that counter after success. They issue no real suspend or sysfs
requests. This verifies orchestration decisions, not kernel wakeup, timer behavior,
sensor continuity, or actual energy savings; those still require battery trials.

Task0295 live-tested the generated bundle for4min after target readiness. Starting
the target activated the prepared profile and gated both recorders before power
coordination. Stopping only the power service stopped the target, recorders and
profile; all archives drained and independent readbacks matched original settings.
Backend provisioning/restoration still used the temporary candidate and independent
fallback timer. This validates bundle integration, not permanent installation or
battery suspend/overnight endurance.

Optional selected endpoint `fsl_wk` adds SSC_WORKOUT_SUID to the generated SSC unit.
The manifest explicitly records workout_summary_enabled, including false when the
endpoint is omitted. Obtain it from current-boot discovery just like the other
endpoints. This adds cached-summary reads, not workout activation or a claim that
valid workout measurements are available.

Use the native controller to produce selection JSON rather than assembling it:

```
hoki-health-recorder --select-processed /private/snapshot/discovery /private/new-selection
```

Pass new-selection/selection.json to session_units.py. Standard18-name discovery
now contains all four implemented processed-reader endpoints, including optional
workout summaries. A missing/ambiguous endpoint or stale boot fails before output
creation; existing output is never overwritten. Native snapshot helpers retain
their20s per-operation bounds.


Task0319 distinguishes healthy in-flight recording from a failed recorder during
suspend admission. With an active owned recording and valid counters, pending
writes/flushes protected by its wake hold cause a skipped attempt and retry,
not a failed child contributing to the three-error session stop. The same
check runs before alarm creation and again after reading wakeup_count. If the
second check defers, a `skipped` result is saved beside the existing intent;
no mem write is issued. No wake hold is forcibly released. Missing wake protection,
invalid counters, stopping, changed identity and storage/input/flush/wake errors
remain fatal to the attempt. Kernel handshake/RTC guards are unchanged.

A successful child exit can mean a deliberate deferral, not successful suspend.
Power analysis counts explicit skipped results separately and never adds them to
measured suspended time. Deferrals before intent creation appear only in the
journal, so intent files are not a complete count of all admission attempts.
This policy is host-tested; actual on-wrist retry behavior still needs validation.

Task0324 adds a distinct child exit76 for a valid wakeup_count commit rejected
with EINVAL, or a mem write rejected with EBUSY. Both publish failed-result
evidence first. This is a retry policy, not a diagnosis of the kernel's reason.
The coordinator retries after one second, stopping after ten such exits without
a zero exit. Retry exits do not clear accumulated hard failures; other failures
retain the three-error stop and five/ten-second backoff. A zero exit resets both
counters and may still mean a healthy pre-suspend deferral rather than sleep.
No wake lock, RTC bound or kernel handshake is bypassed. The watch's systemd-run
was checked to propagate exit76; actual changed-policy sleep/power behavior still
requires a fresh live recording trial.

Task0325 exercised four mem-EBUSY short retries during a completed, restored
combined capture. All22intents had results (18returned,4failed); no hard backoff
occurred. Counter-commit EINVAL was not observed in that run. The two-minute
fuel-gauge estimate increased despite a slightly higher measured sleep fraction,
so this validates retry behavior, not an energy improvement or long endurance.

Task0333 introduces `--suspend-recording-paced` for the matched coordinator.
Legacy `--suspend-recording` retains its zero-exit deferral behavior. The paced
command returns77 for healthy deferrals and returned mem calls with less than
0.5seconds estimated sleep (or invalid timing); the coordinator waits one second.
Only a returned call with at least0.5seconds estimated sleep exits0 and avoids
that fixed delay. Durable result publication must succeed before either outcome.
Both healthy outcomes reset retry/failure streaks; exit76 remains bounded and
never clears hard failures. Existing power, owner, durability, RTC and kernel
handshake checks remain mandatory. The new explicit CLI prevents an older helper
from silently being treated as a paced helper. Host tests pass; live power
improvement and longer endurance require measurement, not inference from pacing.
