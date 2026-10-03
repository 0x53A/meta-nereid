# Hoki health recorder controller

Native root controller for the opt-in sensorfw HIDL1 recording socket. This is
one component of raw and processed recording. The sensor-hub helper is maintained
in [ssc/](ssc/README.md). The bounded profile, recording and suspend orchestration
is described below; these remain opt-in research recording tools.

Build from this directory:

```sh
nix-shell --run "cargo build --release --target armv7-unknown-linux-gnueabihf"
nix-shell -p patchelf --run "bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-health-recorder"
```

The pinned Rust toolchain and Cargo.lock make dependency selection reproducible.

The sensorfw extension writes each HAL capture's `hal/session` JSON before the
first event segment. It records the session and boot identity, sensor inventory,
and storage limits. The decompressed `hal/events-*.bin.gz` segments (legacy `events-*.bin`) use the fixed HOKISEN1
header and 88-byte records; `hal/checkpoint.json` tracks durable bytes and
completion. New capture-start provenance belongs in `hal/session` so older
event decoders can continue reading the same raw format. Existing archives
cannot gain capture-time provenance retrospectively.

Run the complete host regression suite from any directory with
`sh /path/to/hoki-health-recorder/test.sh`. It requires Cargo, Python 3 and a host
C compiler with ASan/UBSan support. The suite covers Rust controller logic,
Python analysis and deployment tooling, offline heartbeat and timestamp decoding, and native
SSC journal/protocol tests. It does not connect to the watch or deploy anything.
For Rust-only checks, use `nix-shell --run "cargo test"` from this directory.
The companion [sensorfw recording tests](../../../sensorfw/tests/recording/README.md)
exercise the backend's queues, storage, arbitration and durability bookkeeping.
Run those separately when changing the backend.

```text
hoki-health-recorder SOCKET NEW_CAPTURE_DIRECTORY SECONDS
```

Both paths must be absolute, with existing root-owned private parent directories.
The capture directory must not already exist. The socket must belong to the
patched sensorfw backend, with its plugin loaded and session ownership support
advertised. Each capture uses a fresh kernel UUID; mutating commands carry it and
open/status responses must identify the same session. Duration0 records until SIGTERM
or SIGINT; positive durations (at most86400s) count from completed activation.

With NOTIFY_SOCKET set, the controller sends systemd READY=1 only after every
selected activation succeeds, backend health is rechecked, and activated handles
plus activation-complete BOOTTIME are durably saved. Use Type=notify and
NotifyAccess=main with a bounded startup timeout. A configured notification
endpoint that cannot accept the message fails startup and triggers the normal
owned drain. Readiness means activation completed, not fresh samples from every
sensor. Without NOTIFY_SOCKET, standalone operation remains supported.

The controller holds a private file lease beside the socket. It inventories all
HAL types, prefers their wakeup variants, rejects ambiguous descriptors, duplicate
or unsupported handles, and demands outside the backend's timing limits, and
clamps the research 25 Hz/5 Hz rates to advertised limits. Ordinary manual
captures request immediate event delivery and flush every 10 seconds when
coordinated with powerd (20 seconds without it). The opt-in buffered full-profile
trial is documented below. The service requests no per-capture byte ceiling and a250MiB free-space reserve.
It requests clean shutdown at258MiB available to leave finalization headroom;
startup requires266MiB. The backend independently checks available space on
writes. Standalone callers may still select explicit legacy byte budgets.
A CLOCK_BOOTTIME_ALARM timer supplies fallback wakes without continuous polling;
short polling is used only for startup and explicit checkpoint completion.

Final stop must report healthy storage, zero drops/input failures, matching
received/submitted/durable counts, and no recorder wake hold. Controller metadata
is atomically persisted with planned descriptors, activated handles, boot identity,
end reason and final response. Interrupted or failed runs cannot claim successful
controller completion. Opening an existing/unacknowledged capture does not grant
authority to stop it.

Crash cleanup is available as:

```text
hoki-health-recorder --cleanup SOCKET CAPTURE_DIRECTORY
```

It acquires the same controller lease (exit75 if busy), reads private saved
metadata and compares boot/session identity with the backend. Only an exact
match may drain the session. An in-flight flush may delay stop for up to12s;
final checkpoint completion has a separate12s bound. `recovery.json` records the
result separately; `controller.json` is never rewritten to disguise a crash as
a normal completion. A mismatch records `not_current` without mutating the
backend. Other errors return1. Once the lease and private capture metadata are accepted,
backend query, identity validation and drain failures persist a failed recovery
result with diagnostics and any initial status. Earlier path/lease/metadata
failures and failures writing the recovery record require supervisor stderr
preservation. Every invocation replaces the latest recovery result; it is not an
attempt history. Metadata publications use unique temporary filenames so an
interrupted write cannot block the next controller or recovery update. Abandoned
`.pending` files remain unpublished evidence; only the named `.json` is committed.
See tasks0257–0258 for live recovery validation.

Ordinary recording requests immediate HAL delivery and reports checkpoint
readiness/deadlines to powerd; it does not directly invoke the kernel suspend
interface. The manual unit's explicit `HOKI_BUFFERED_FULL_TRIAL=1` opt-in enables
a finite full-profile latency probe at 7, 20 or 40 seconds, with a fallback
flush/deadline 10 seconds later. FIFO reservation metadata classifies each
stream's risk but does not clamp the experiment; continuity and loss must be
measured. The trial defaults to 300 seconds, is capped at 1800 seconds, and
suppresses the supervisor's periodic battery/storage poll. Its start/end battery
snapshots are not a continuous safety monitor. Ordinary Settings captures keep
immediate delivery and the existing monitor. The separate `--suspend-recording`
command and session-owned
coordinator described below apply their own identity, durability, power and
wakeup-count guards. Merely building this source tree does not install or enable
services. Do not infer complete SoC stream coverage, sensor FIFO continuity or
battery endurance from controller tests.

See task0255 for validation status. On-watch deployment needs a bounded service
and independent restoration until lifecycle/failure behavior is verified.

SSC endpoint selection for a successfully completed discovery service:

```text
hoki-health-recorder --select-ssc DISCOVERY_DIRECTORY fsl_min
```

The root-only command reads bounded, root-owned private regular JSON files
without following final-component symlinks. It requires current boot identity,
valid inventory/session schema, no interruption/parser error, a finalized clean
SSC archive, and exactly one response/ID for exactly one matching entry. Output
is a canonical36-character protobuf SUID plus newline, suitable as one argument
or an environment-file value. It does not claim freshness, algorithm activation,
or exhaustive discovery. The caller must first require successful discovery
process exit; visible files alone cannot prove its final directory fsync succeeded.
Task0267 exercises this flow with a new discovery before combined service startup.

See [CAPABILITIES.md](CAPABILITIES.md) for measured discovery/attribute coverage
and the explicit gaps between advertised endpoints and verified recording.

A prepared sleep configuration plan can be created without changing the watch:

```text
hoki-health-recorder --plan-sleep BASELINE_DIRECTORY NEW_PLAN_DIRECTORY
```

The baseline contains successful `discovery`, `user`, `tracking`, and `detect`
captures. Each getter must have exited successfully; the caller must preserve
that evidence. The planner validates boot/source/mode/event identity, archive
completion, and supported canonical boolean flags. It saves a private, durable
`plan.json` with narrowly scoped request payloads, exact original/expected replies,
and reverse restoration order. It preserves nested/unknown settings, omits flags
already enabled, and never overwrites an existing output directory.

The plan is **prepared only**. An executor still must own the configuration
lifecycle, revalidate the baseline immediately before mutation, arm independent
restoration, verify readbacks and preserve failure evidence. Those execution
steps are not implemented by `--plan-sleep`. The profile covers sleep/RHR
permissions and top-level tracking/detection; it does not enable every processed
or optical subfeature and does not assert sleep-classification validity.

The `sleep_transaction` library now implements intent-before-write activation and
reverse restoration, including uncertain sends and readback conflicts. It rebuilds
the plan from saved discovery/configuration evidence before recovery and rejects
wrong boot/owner identities. Already-enabled settings remain unowned. Interrupted
activation restores rather than resumes. The live backend and bounded service
commands below supply profile ownership, supervised getters, durable checkpoints
and a separate restoration process.
Older prepared plans lacking discovery evidence must be regenerated. Tests inject
failures at every activation/recovery boundary; that mock coverage does not
validate the live adapter's process or filesystem behavior. See task0273.

`--snapshot-sleep HELPER NEW_DIRECTORY` now acquires discovery plus all three
configuration baselines through bounded transient systemd services. Each capture
retains supervisor logs/results and raw SSC records. It fails on unsuccessful
helper lifecycle, stale/mismatched identity or incomplete archive counters. The
new directory must have a private root-owned parent. Use its output as the input
to `--plan-sleep`; this path makes no configuration changes. See task0274 for
read-side validation and the bounded trial commands below for writes.

The `sleep_backend` adapter implements the transaction engine's getter, setter
and persistence operations using supervised helpers. Setters additionally reload
the committed journal and require an exact matching intent. A process-wide profile
lease excludes other live adapters. It is used by the bounded lifecycle below;
helper quiescence precedes restoration. See tasks0275–0278.

Configuration ownership now has a persistent reservation in
`/var/lib/hoki-sleep-profile/owner.json`, in addition to the process flock. It
survives adapter death and binds the boot, owner and canonical capture path.
Only verified, durably committed restoration releases it; failed release can be
retried without repeating sensor writes. A different boot is refused pending
reconciliation. Production session journals must also use persistent storage.
The bounded lifecycle below adds service fallback and helper quiescence.

Bounded sleep configuration service trials now have native commands:

```
HOKI_SSC_HELPER=/absolute/collector HOKI_SLEEP_SECONDS=10 \
  hoki-health-recorder --prepare-sleep /private/plan /private/new-session
hoki-health-recorder --launch-sleep /private/new-session OWNER_UUID
```

Preparation prints the owner and writes private transaction/runtime metadata and
both service definitions. Use persistent storage for sessions. Launch links these
units at runtime, verifies resolved fragment paths, then starts activation. Only
the expected active service MainPID may execute the internal `--apply-sleep` or
`--restore-sleep` commands. Apply always queues a separate recovery service on exit;
recovery orders after apply termination and explicitly quiesces old helpers before
restoring. Recovery unit names include an attempt UUID to avoid historical-name
ambiguity. HOKI_SLEEP_SECONDS accepts1..86400s (default30) after activation, using a CLOCK_BOOTTIME_ALARM
one-shot armed before publishing readiness. Time spent suspended counts toward
that duration, and the timer can wake the CPU for recovery. Alarm creation must
succeed before activation; arming/wait errors still queue independent recovery.
The apply service bound is max(600, duration+420) seconds, allowing activation
time before the requested active duration. Recovery retains its separate360s
bound; both have15s stop deadlines. An8h profile therefore has an8h7min apply
service bound. This supports bounded long sessions; it does not establish8h
recording capacity, sensor continuity, or battery endurance. No automatic restart loop is installed.
This is a configuration trial, not the final sensor recorder or CPU sleep policy.
Missing journals, cross-boot reservations and failed recovery still require
reconciliation. Task0278 verified a normal10s cycle: all three configurations
returned exactly to independent fresh baselines, both services exited successfully,
and persistent ownership released. Task0279 also verified full-lifecycle SIGKILL recovery both after activation and
while a setter was still uncommitted in the controller journal. Independent final
baselines matched, including the case where the uncertain write had applied.
Recording/suspend integration and cross-boot reconciliation remain outstanding.

`--await-sleep SESSION OWNER_UUID` gates recorder startup on an active validated
transaction and a readiness marker matching the live config service's PID and
invocation. The marker is produced after the active checkpoint is durable. Task0280
verified this gate with simultaneous HAL and SSC collection, explicit recorder drain
before restoration, and restoration of the original sensorfw environment. All29 HAL
types activated;10,068 HAL records and3 complete SSC minute transfers were archived
without reported loss. On-charger freshness and the hub's backdated clock remain
unverified; deliberate CPU suspend and permanent session packaging are still pending.

Task0282 additionally verified failure coupling: placing the configuration apply
service in `PartOf` the capture target makes a HAL controller crash stop both
recorders and trigger configuration recovery. Recovery orders after both recorder
services and the readiness gate. In that trial, SIGKILL left truthful failed
controller metadata while the separate cleanup drained all4357 HAL records; SSC
finalized33 records. Independent reads matched the original configuration. These
unit relationships currently live in the trial fixture; permanent session
packaging and collector-crash validation remain outstanding.

`--suspend-recording SOCKET CAPTURE_DIRECTORY` performs one bounded recording-aware
suspend attempt. It requires root and an active `Type=exec` unit named
`hoki-recording-suspend-UUID.service`, passed as `HOKI_SUSPEND_SUPERVISOR`, whose
MainPID is the caller. Required limits are RuntimeMaxSec=30, TimeoutStopSec=5,
KillMode=control-group. `HOKI_SUSPEND_SECONDS` defaults10 and accepts3..20.

Charger or connected USB causes an explicit skipped result before recording is
validated. On battery, the command requires activated, same-boot/session metadata
and healthy fully durable backend counters. It arms CLOCK_BOOTTIME_ALARM, fsyncs
an attempt-intent file, reads wakeup_count, rechecks recording and power, commits
the wakeup-count handshake, and checks that at least2s remain on the alarm before
requesting mem. The service deadline bounds a blocked wakeup-count read. Separate
intent/result files retain uncertainty if killed; a result reports BOOTTIME minus
MONOTONIC residency and alarm expiry rather than assuming a successful sleep.
Returned results also persist `start_boottime_seconds` and `end_boottime_seconds`
from the readings used for elapsed time. These bound the measurement, including
logging and syscall overhead; they are not exact kernel sleep entry/exit times.
Older captures have durations only and need matched journal attempt records to
place those measurements on the BOOTTIME timeline. Intent time is earlier and
must not substitute for measurement start.

This is a single-attempt component. The caller still owns UI/radio policy, target
lifetime coupling, retries/backoff and recurring scheduling. It never releases
vendor or recorder wake holds. Charger skip and supervisor checks can be tested
on USB; actual residency with this component needs off-charger validation. Earlier
standalone suspend experiments do not substitute for that end-to-end check.

`--select-processed DISCOVERY_DIRECTORY NEW_SELECTION_DIRECTORY` validates the
saved inventory and final archive status against the current boot, requires unique
fsl_min/fsl_sleep/fsl_rhr/fsl_wk entries, then atomically publishes private
selection.json for the session-unit generator. It validates every endpoint before
creating the output directory and refuses existing output. The file includes the
discovery session identity and explicitly scopes itself to implemented processed
readers. It does not claim enumeration of all SSC features or measurement freshness.

`--reconcile-sleep PRIOR_PROFILE NEW_OUTPUT_DIRECTORY` provides a conservative
recovery path for a reservation left by an earlier boot. It holds the profile
lock, reopens only the exact existing reservation, and takes fresh discovery and
all three configuration snapshots. It releases the reservation only if the full
payloads match the saved pre-session baseline. Firmware identifiers may change
across boots, but every getter must match fresh discovery on its own boot.
No firmware setters are sent, and the original transaction remains unchanged.
A mismatch, incomplete evidence, or missing ownership retains the reservation
or fails without creating one. This does not restore settings that persist changed
across reboot.

Run this as root in a fresh `Type=exec` service named
`hoki-sleep-restore-OWNER_UUID-ATTEMPT_UUID.service`, setting
`HOKI_SSC_SUPERVISOR` to that exact name. Required bounds are RuntimeMaxSec=120,
TimeoutStopSec=10 and KillMode=control-group. The command verifies its MainPID
and service properties. Use absolute paths and a new output directory under a
private root-owned parent. It saves durable intent, fresh raw evidence and an
assessment before removing ownership; a final result records completed removal.
An interrupted attempt must be interpreted from these records and the actual
reservation, never from a missing final result alone. No automatic boot retry is
installed. Task0300 covers host policy/ownership tests and the ARM build; live
reboot and interruption validation remain pending while task0298 records.

### Offline HAL timing analysis

`python3 tools/verify_hal.py CAPTURE/hal` checks checkpointed bytes and reports
per-channel source intervals and delivery ages. `tools/hal_coverage.py` also
compares those records with selected and activated channels. Use stable saved
captures and verify their source hashes independently.

Both tools accept `--source-window-ns START END` to add statistics restricted to
an inclusive source-timestamp interval. This is useful when an initial cached
sample predates activation and dominates the full-archive maximum gap. Supply
integer nanoseconds in the same clock domain as the source timestamps; do not
substitute wall-clock timestamps. The controller's activation-complete and end
boottimes can define a recording window once that clock relationship is checked.
Samples received during activation may legitimately precede that first marker.

The additional `source_window_statistics`, `source_before_window` and
`source_after_window` fields preserve the original full-archive statistics,
record counts, hashes and integrity checks. Metadata records are not windowed.
Coverage output counts and requested-period comparisons still describe the full
archive. Being inside the window does not establish physiological freshness,
accuracy, or sample continuity.

## Everyday collection profiles

The [everyday sleep system](../hoki-powerd/SLEEP.md) adds off/daily/sleep/activity/full
capture presets, selected in Settings independently of the sleep switch. The
boot-enabled `hoki-health-policy.service` starts only its separately owned profile
recording unit. The persisted default is off. Selecting a profile starts a new
capture; an enabled profile starts a new capture on subsequent boots as well.
The manual recording toggle remains independent and exclusive setup ownership
prevents one service from cleaning up the other's capture.

The native controller holds a powerd inhibitor during setup and durable
checkpoints, then reports readiness and its next maintenance deadline with a
fallback alarm already armed. Ordinary profiles request immediate HAL delivery;
non-wakeup channels retain a CPU inhibitor. These presets do not promise low CPU
duty cycle or continuous health metrics. Existing storage/battery admission and
stop limits remain in force. Failed/stopped profiles do not automatically retry
until the profile is changed.

The manual recording service also supports an explicit bounded full-profile
buffering probe, separate from Settings defaults. Set
`HOKI_BUFFERED_FULL_TRIAL=1`; optionally choose
`HOKI_BUFFERED_FULL_TRIAL_LATENCY_SECONDS=7|20|40` (default 7) and
`HOKI_BUFFERED_FULL_TRIAL_SECONDS=1..1800` (default 300) in the service-manager
environment before starting the manual unit. Each latency step gets a fallback
flush ten seconds later. The controller records per-stream periods, requested
latency, live FIFO metadata when available, and wake-held samples at checkpoint
boundaries. Requests are deliberately not clamped to advertised FIFO counts, so
the trial may reveal gaps or loss. The Settings toggle never enables it. Current
watch inventory output lacked FIFO fields; until the updated sensorfw backend is
deployed those capacities will be marked unknown rather than joined from a stale
inventory snapshot.

For a finite delivery-isolation experiment, set
`HOKI_BUFFERED_TRIAL_SELECTION=continuous-only` with the same buffered-trial
opt-in. This removes non-continuous channels, including derived heart-rate
streams, and retains the continuous channels' full-profile rates and latency.
The default is `full`. Session/controller metadata records the selection;
controller `full_profile_coverage` is false for the reduced selection. The
historical `buffered_full_trial` flag identifies the trial machinery and alone
does not promise full coverage. A reduced selection is rejected outside a
buffered trial. Both selections retain the same durability and fallback gates.

For narrower optical isolation, `HOKI_BUFFERED_TRIAL_SELECTION=ppg-motion`
selects only accelerometer (type 1), gyroscope (4), and raw PPG (65572).
`ppg-motion-hr` adds only heart rate (21); `ppg-motion-spo2` adds only SpO2
(65561). Missing required types reject the trial. Shared channels keep identical
rates/buffering; added derived channels keep immediate delivery. These finite
experiments record their selection and do not provide full-profile coverage.

### SpO₂ attempt scheduling

Ordinary full captures now default to `HOKI_SPO2_POLICY=periodic`: activate SpO₂
for up to 180 seconds, releasing early after an accepted result. Attempts start
15 minutes apart, regardless of success or failure.
Durable results are checked during existing flush cycles, so reaction/window end
can be delayed by checkpoint work. An accepted result requires the selected
handle, fresh source timestamp, FINAL state, confidence >=80, signal state 0,
and integer-truncated value >80 (also rejecting nonfinite/out-of-range fields).
These stock-consumer checks do not establish medical accuracy.

Daily, sleep and activity profiles still do not select SpO₂; this policy does not
add it. Explicit buffered trials retain continuous SpO₂ requests for comparison
and reject non-continuous policy overrides. `HOKI_SPO2_POLICY=continuous` restores
continuous ordinary full collection; `off` excludes the recorder's SpO₂ demand.
Other sensor clients may independently keep the optical hardware active.

Periodic captures declare `full_profile_coverage=false`, record policy/deadlines,
and append acknowledged demand transitions to `hal/spo2-transitions.jsonl`.
The supervisor supplies a shared private cooldown file, retaining the last attempt start
across service restarts (including failed or interrupted attempts). Same-boot timing uses BOOTTIME;
after reboot it uses wall time, conservatively waiting fifteen minutes on rollback.
Standalone controller use without `HOKI_SPO2_COOLDOWN_FILE` has capture-local
cooldown only. Interrupted/failed attempts do not count as successful samples;
late wakes never cause a burst of catch-up attempts. Report/raw event counts can
exceed six per hour: the limit concerns successful scheduled attempts, not every
intermediate estimate emitted during those attempts.

## Shared activity consumers (2026-09-28)

Ordinary Settings manual recording now holds a `full` consumer lease rather than
creating its own capture. Settings' automatic profile is another consumer.
`health-policy` exposes `/run/hoki-health-policy/control.sock` to ceres/root:
newline JSON `acquire` with `profile`, `status`, and `release`. Ownership belongs
to the connection; reconnecting creates a new consumer. Clients poll within
30 seconds. Supported app profiles: running, spo2, daily, sleep, activity, full.
Immediate spo2 conflicts with running and returns busy in either acquisition
order. Dead clients release only their own request.

The private `demands.json` plan has a per-policy-process UUID epoch and revision.
The native controller applies its union during one open capture, then reports
that epoch/revision only after a durable checkpoint. GUI clients must wait for
`ready`; an accepted request is not a hardware acknowledgement. Transitions are
retained in `consumer-transitions.jsonl`. Readiness expires after five seconds.
Starting the first consumer and stopping the last still use the existing
sensorfw setup/cleanup lifecycle. Intermediate changes do not restart sensorfw.

The broker owns periodic optical windows. Running includes heartbeat and RR,
which are explicitly released before SpO2 acquisition and restored afterwards.
Full collection remains additive. A manual SpO2 client reads raw timestamped
results through the broker; it never independently starts the HAL sensor.
Unmodified external sensorfw clients remain outside this optical policy.

The PoC uses one-second durable checkpoints and conservatively holds the recorder
CPU inhibitor. Finite buffered experiments retain their explicit isolated manual
path and refuse setup while a shared capture owns sensorfw. Ordinary manual
service stops release a lease; they do not stop another consumer's recording.

### Configurable subscription rates

`acquire` also accepts `rates_hz`, keyed by decimal HAL type IDs, for adjustable
continuous channels already in the requested profile. For example:

```json
{"command":"acquire","profile":"running","rates_hz":{"1":10,"4":10}}
```

Type1 is calibrated acceleration,4 gyro,9 gravity,10 linear acceleration,
35 uncalibrated acceleration and65572 raw PPG. Full also permits2/14 magnetometer,
6 pressure,11/15/20 rotation vectors and16 uncalibrated gyro. Running
and activity permit1/4; sleep permits1. Daily and SpO2 have no adjustable types.
Overrides replace that subscription's default rate, not another consumer's rate.
Running defaults to50Hz for acceleration/gyro. Full defaults to the advertised
maximum of every continuous channel (50Hz motion/magnetometer/rotation vectors,
25Hz pressure, approximately26Hz PPG on hoki). Channels without an advertised
maximum retain bounded defaults; event-driven channels retain their event semantics.
This also applies to explicit buffered full trials; FIFO capacity in seconds shrinks
at higher rates. Daily/sleep/activity defaults are unchanged.
Missing overrides retain defaults. Reacquiring on one connection atomically
replaces its previous profile/rates; closing it removes only that subscription.

The union selects the shortest period for each shared hardware handle. A10Hz
subscriber therefore shares50Hz acquisition while a50Hz subscriber is present;
there is no per-consumer downsampling. Rates reduce to the fastest remaining
request on release, without restarting capture. Rates are finite numbers between
0.1 and1000Hz and are bounded to the selected descriptor's advertised limits.
Event-driven channels such as HR/steps are not promised a periodic delivery rate.

Every client's acquire/status response includes `applied_rates_hz` once the
native controller acknowledges the current revision (`ready:true`). Keys are
HAL type IDs. During transitions, absent/stale rates with `ready:false` must not
be treated as acknowledgement of the new request. `observed_rates_hz` separately
estimates cadence from recent durable source timestamps after the latest demand
transition; absent means insufficient fresh samples, not zero. Hardware may
quantize requested rates. These estimates use at most5seconds/the last4096
records of the current segment and require3samples spanning at least0.5seconds.
They describe acquisition, not client polling rate or guaranteed future delivery.
Raw samples remain in the shared capture; this control API is not a raw event
stream. Existing profile-only clients remain compatible and see the same rates.

Planned raw-PPG processing, quiet/sleep windows and export contracts are recorded
in [PROCESSING.md](PROCESSING.md). The supported planning baseline is approximately
25Hz PPG; higher HAL requests have not increased observed delivery.

### Shared Full buffering (on-watch validation)

`HOKI_SHARED_FULL_BUFFERED=1` on `hoki-health-profile-recording.service` enables
seven-second, period-aligned batching for continuous channels when every active
consumer is Full (or Off). Other reporting modes retain immediate delivery.
The daemon retains Full's maximum rates and periodic SpO2 policy. A Running,
manual SpO2, or other-profile consumer restores immediate delivery and one-second
maintenance. A new subscription can wait for the existing buffered wake interval
before the controller applies it; clients must await acknowledged readiness.

The exact applied plan must pass the existing wakeup-descriptor and FIFO
classification checks. Suspend permission also requires a healthy owned capture
with all received data durable and its recording wake hold released. A fallback
alarm is armed before reporting readiness, at most 8 seconds away; the next
optical transition may shorten this interval. Shared status validity is bounded
at 13 seconds for the current eight-second maintenance interval. This does not bypass powerd's display,
radio, USB or charging gates. Wi-Fi must be off for the ordinary suspend path.

This switch defaults off. FIFO reservations are recorded/classified, not a proof
of lossless delivery; deep-suspend residency and overnight endurance require
on-watch measurements. The finite legacy buffered-trial mechanism remains
separate.


### Streaming gzip and crash boundaries

New captures stream level-1 gzip directly from the storage worker into
`events-NNNNNN.bin.gz`; no intermediate uncompressed sample files are written.
Segments rotate at the existing4MiB logical size for bounded decoding; that is
not a limit on total capture duration. Compression stays off the HAL poll thread.

Each checkpoint finishes a gzip member, syncs the data file, then atomically
publishes/syncs checkpoint metadata. `segment_bytes` and `total_bytes` still
count decompressed HOKISEN1 bytes. `compression: "gzip"`,
`compressed_segment_bytes`, and `compressed_total_bytes` describe physical disk
bytes. Live and recovery readers limit decompression to the committed physical
prefix, validate gzip checksums, and ignore any subsequent incomplete member.
A completed archive also closes gzip before publishing its final checkpoint.

A hard interruption can lose everything since the last completed checkpoint,
including sensor FIFO/queue/compressor data; it is not guaranteed to lose only
one or two samples. This relies on the filesystem/device honoring fsync. Old
uncompressed archives remain readable by the updated verifier and analysis tools.
