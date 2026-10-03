# Gesture wake investigation bundle

Run only with explicit authorization and exclusive access to the watch's sensors.
The initial bounded live screening was authorized on 2026-09-27; its results are
summarized below. No service is
installed, enabled, or started by the build. This is research instrumentation,
not a shake-to-wake feature or a battery-life claim.

The existing HAL advertises significant motion (17), wrist tilt (26), and motion
detection (30). All emitted in the earlier overnight HAL capture, but controlled
gesture response and suspend wake remain unverified. Start with wrist tilt;
also test an actual shake, because tilt success is not shake success. Standard
HAL activation does not expose an arbitrary acceleration threshold. Direct SSC
sig_motion/wrist_tilt_gesture discovery is corroborating inventory only; this
bundle does not guess vendor configuration messages or modify firmware.

## Local preparation

```sh
cd meta-nereid/tools/gesture-wake
python3 -m unittest -v test_probe.py
bash build.sh
```

Build runs from the LP watchface's Nix environment with a separate Cargo target
directory, builds only the two research binaries, patches the ARM interpreter
and RPATH, and writes `build/bundle/SHA256SUMS`. Copying the complete bundle later
does not replace installed programs. `hoki-health-record describe` is the only
recorder change: it lists current descriptor metadata without activation,
batching, flushing, or consuming the shared event queue. Other modes retain
their existing behavior. The recorder uses a separate bounded poll process,
auto-rearms one-shot sensors and supports a durable stop file.

## After authorization: handoff and deployment

1. Confirm the other session's recordings and suspend experiments are finished;
   preserve its captures before any disruptive action. Obtain exclusive sensor
   ownership for the trial, including sensorfw app users. Stopping sensorfw
   interrupts those users even though the transient unit restores it afterward.
2. Record current sleep/radio/display settings and running services. Through
   normal Settings, disable automatic sleep and select sensor profile off. Stop
   an active manual recording through its owning workflow, wait for completion,
   and verify saved data. The harness refuses active recording units and never
   stops them for you. It changes no persistent policy, radios, or display mode.
3. Copy the bundle to a root-owned, non-writable-by-others directory on the watch,
   such as `/opt/hoki-gesture-wake`. Verify `sha256sum --check SHA256SUMS` there.
   Python 3, systemd-run, busctl with JSON support, libgbinder, the HIDL sensor
   service and the existing powerd status socket are required. Missing
   prerequisites are errors, not reasons to bypass a guard.
4. Keep sensorfwd initially active. Each trial records restoration intent, stops
   sensorfwd, confirms no remaining MainPID (a failed shutdown state is retained
   as evidence), ensures the vendor HAL is running, queries current descriptors,
   selects exactly one matching wake-up
   type, and restores sensorfwd in independent systemd `ExecStopPost`. Normal
   completion requests durable recorder shutdown before that cleanup. Abnormal
   exit retains partial evidence and systemd terminates the whole process group.
   Check `cleanup.json`, the unit result and sensorfwd before another trial.

No deployment command is executed by any host test or build script.

## Awake screening

For a combined waveform capture, add `--raw-accel --accel-batch-ms 20000`.
The selected wake-up accelerometer requests25Hz (subject to live descriptor
limits) and20s buffering; the gesture still requests zero latency. Raw baseline
mode records acceleration alone. `--post-resume-seconds 10` retains a tail before
the final flush, allowing the plot to show movements after an early unrelated
wake too. The cleanup deactivates both selected types. The raw descriptor,
requested latency, source timestamps, arrival timestamps and exact payloads are
retained; inspect ACTIVE lines to verify both requests were accepted.

Keep a raw-data baseline alongside the gesture trial: raw buffer delivery may
itself cause a wake. A shorter RTC fallback than the raw batch latency can reduce
that ambiguity, accounting for time already spent gathering before entry.
No gesture-caused wake is inferred from the raw waveform or early return alone.
Analyze raw source continuity and delivery age separately; FIFO claims and a
successful batch request do not establish loss-free buffering on their own.

The following commands are **watch-only, after authorization**:

```sh
cd /opt/hoki-gesture-wake
python3 probe.py launch --confirm-exclusive-handoff --mode awake --type 26 --seconds 30 --action shake
```

Launch returns the unique capture directory and starts a bounded transient unit.
Use its journal to see `READY`; the observation window starts five seconds later.
Use a separate clock/video to annotate actual gesture times, not network commands
sent to the watch during suspend. The action option is the intended action, not
evidence that it occurred. Capture root is `/var/lib/hoki-gesture-wake` (private).

Screen each of types 26, 30, 17 separately. Perform annotated shake and wrist-tilt
actions, with stillness between attempts; repeat starts to exercise activation
and rearm. Include still and ordinary arm-swing controls. Significant motion may
require locomotion and may be unsuitable for a quick wrist shake. Do not proceed
with a detector that cannot reliably recognize the intended action while awake.

## Isolated suspend trials

Use a delayed local launch so there is time to disconnect USB/charger, turn
Wi-Fi off through normal controls, and turn the display off. Use screen-off for
this isolated trial: disabling automatic sleep also exits managed ambient mode.
The runner requires discharging, USB disconnected, Wi-Fi down,
`pm_test=[none]` (or live kernel configuration proving CONFIG_PM_DEBUG disabled
when that file is absent), mem support, automatic sleep disabled, sensor profile off, no
CPU inhibitors, and no logind sleep/idle inhibitors. The checks repeat immediately
before the helper. Leave those settings stable for the trial. This is an explicit
research suspend owner while everyday automatic suspend is disabled; it does not
change/bypass powerd's production suspend gate or release vendor wake locks.

Example after handoff, scheduled locally while still connected:

```sh
systemd-run --unit=hoki-gesture-delayed --on-active=60s \
  /usr/bin/python3 /opt/hoki-gesture-wake/probe.py launch \
  --confirm-exclusive-handoff --mode baseline --seconds 20 --action still
```

Use a fresh delayed unit name for each run. Its timer starts the launcher before
suspend; it does not mark the gesture or intentionally wake the suspended trial.
Baseline arms no detector. Repeat with `--mode suspend --type 26 --action shake`
only after the awake screen passes. During the external trial window, wait until
roughly five seconds after expected entry, perform the action once, then stay
still. Do not press buttons, touch the screen, or reconnect USB until the 20-second
fallback deadline plus cleanup time has passed. A video/external clock is needed
to reconstruct whether the action actually occurred during sleep. Without that,
mark the attempt unscorable rather than a miss or success.

The existing `hoki-suspend-check mem` arms a CLOCK_BOOTTIME_ALARM fallback,
performs the kernel wakeup_count handshake, rejects an expired/near-expired
alarm, and reports elapsed time minus monotonic elapsed time as estimated suspend
residency. The supervisor bounds blocked preparation and cleanup. Do not replace
it with a bare sysfs mem write. A >=0.5-second estimate establishes useful suspend
residency for this test, not every hardware power domain's deepest state.

Run at least five baseline trials, then ten annotated gesture trials per viable
candidate plus ten still/ordinary movement controls. This is a feasibility screen,
not sufficient statistical evidence for a production reliability promise. If
promising, follow with at least 100 varied intentional gestures, extended normal
daytime movement and matched detector-off/on power measurements. Revisit numerical
acceptance limits with Lukas before treating them as product requirements; a
working awake screen alone never qualifies.

## Evidence and interpretation

After the watch is available again, copy each complete private trial directory
into the host repository's ignored `data/20260927_Gesture_Wake/`, verify transfer
hashes, and analyze locally:

```sh
python3 analyze.py /path/to/copied/trial-UUID
```

Retained evidence includes boot identity, selected current descriptor, raw 64-byte
HAL payloads with source/arrival timestamps, one-shot rearms, observation bounds,
suspend return/alarm status, before/after wake-source counters, available kernel
suspend statistics/resume reasons, charge-counter samples, and cleanup logs.
Add external action annotations separately and preserve the original files.

- `awake_events_only`: establishes delivery while awake, not suspend wake.
- `fallback_alarm_expired`: cannot credit a sensor wake, even if a queued event
  arrived at the alarm wake.
- `early_wake_with_correlated_sensor_event`: candidate evidence only. Compare
  source/arrival times, wake-source counter deltas and kernel journal; distinguish
  a sensor interrupt from fuel-gauge, radio, button or other wake sources.
- `early_wake_without_matching_sensor_event` or `baseline_early_wake`: investigate
  unrelated wakes before scoring gesture reliability.
- `suspend_not_established` or `incomplete_trial`: unscorable.

The source-time window includes suspend preparation overhead; source timestamp
clock equivalence and exact entry need confirmation from kernel/external timing.
A handshake race may reject suspend when motion fires during entry; it is neither
a failed sleeping gesture nor proof of a wake. Analysis never automatically marks
causality proven. The probe intentionally leaves the display off on resume;
end-to-end display latency is a later integration test after hub wake passes.
Two short charge-counter samples are not a battery comparison.

Afterward restore the manually saved sleep/radio/display settings and verify
normal UI sensors, button wake and any recording service explicitly requested.
Do not automatically restart the other session's capture. Keep all trial data.

## Initial live screening, 2026-09-27

The live run required two preparation fixes: use version1 in the power socket
request, and accept a failed sensorfwd shutdown only with MainPID0. The kernel
lacks pm_test because its live configuration disables CONFIG_PM_DEBUG; that
alternative now requires explicit configuration evidence. Fifteen host tests
cover these cases and the existing guards.

After discarding two unannotated trials whose movement cues were missed, the
prompted awake runs recorded four wrist-tilt events and one motion-detect event.
All four suspend attempts logged kernel deep entry. Baseline slept20.847s and
woke on the fallback alarm. First tilt trial slept16.628s, returned before the
alarm and delivered one fresh tilt event; wake-source counters did not uniquely
identify a sensor-caused wake. The motion/shake trial slept20.318s and expired
the fallback alarm. A second tilt trial slept20.890s, expired the alarm and
recorded no tilt event. These are few human-cued trials, not a measured success
rate, timing-calibrated recognition test or power comparison.

Result: deep suspend is available, but reliable gesture wake is **not established**.
Do not enable an awake-only or intermittently working feature on this evidence.
Type17 was inventoried but not activated in this initial screening. Original
automatic-sleep/sleep-profile configuration and normal recording were restored;
Wi-Fi was restored after every offline trial. Full evidence and limitations are
in [the live task](../../../_Tasks/20260927_Gesture_Wake_Live/summary.md).
