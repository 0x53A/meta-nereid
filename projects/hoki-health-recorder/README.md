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
clamps the research25Hz/5Hz rates to advertised limits. Batch latency and periodic
flush interval are20s. It requests a1GiB data budget with256MiB free-space reserve.
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

The default recording command does not request system suspend. The explicit
`--suspend-recording` command and session-owned coordinator described below apply
separate identity, durability, power and wakeup-count guards. Merely building
this source tree does not install or enable services. Do not infer complete SoC
stream coverage, sensor FIFO continuity or battery endurance from controller tests.

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
