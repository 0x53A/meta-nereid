# SSC recorder component

Maintained source for the Android/bionic helper that collects Qualcomm sensor-hub
QMI traffic alongside the native HAL controller. The old task source paths are
compatibility includes; edit this directory. Source provenance is recorded in
task0259. This is still a bounded research collector, not an installed daemon.

From the recorder project directory:

```sh
bash ssc/test.sh
bash ssc/build.sh
```

The host tests use ASan/UBSan and cover durable journal limits/fsync failure,
queue overflow and in-flight writes, exclusive ownership, minute-transfer
framing, clock encoding, and persistent completion/crash status. The ARM build
uses Android NDK29.0.14206865 by default; set ANDROID_NDK_ROOT for another location.
It emits the executable, SHA256 manifest and compiler version under ignored
ssc/build/. Its interpreter is `/system/bin/linker`; do not apply the Rust
component's glibc patchelf step to it.

Run with `LD_LIBRARY_PATH=/vendor/lib:/system/lib` and SSC_JOURNAL_DIR pointing
to an existing private root-owned capture directory. Each capture exclusively
creates its journal/status files, with a16MiB default journal limit for one-shot/discovery modes,
128MiB for continuous modes, and a256MiB filesystem reserve. The4096-descriptor queue with a shared4MiB byte ring plus one in-flight record preserves exact requests,
responses and indications; overflow and I/O failures poison archive success.
Status distinguishes durable archive completion from unverified protocol and
sensor completeness. Never infer fresh samples from a cached response alone.

No arguments performs bounded discovery of18 explicitly listed data types and,
after raw archive finalization, publishes private `inventory.json` using exclusive
creation, file fsync and directory fsync under the collector wake hold. It records
boot/session identity, scope, parser/interruption flags, and every returned SUID.
`unique`, `ambiguous`, `empty`, and `no_response` are distinct. Empty means the
queried name was not advertised in this run; it is not proof of hardware absence.
Malformed, duplicate or source-invalid replies cannot establish a usable unique
selection. Raw indications remain in the journal. Consumers must validate current
boot, clean discovery completion and a unique selected entry before using an ID.
This inventory is a queried subset, not an enumeration of every possible SSC
firmware data type. The native controller provides --select-ssc to validate and select an endpoint;
task0267 uses it after successful fresh discovery in combined startup. Permanent
launcher packaging remains pending. Read modes are `--attributes SUID`,
`--minute-read SUID`, `--minute-poll SUID`, `--tracking-config SUID`, and
`--detect-config SUID`. SUIDs must come from discovery on the current device.
The `--minute-poll SUID` mode is a bounded60s research run;
`--minute-record SUID` repeats until SIGTERM/SIGINT or an archive/transfer error.
Both use30s intervals and refuse a new GET while the previous transfer remains
incomplete. Set `SSC_JOURNAL_LIMIT_BYTES` to a decimal byte count from16MiB through1GiB
to override the journal budget. The selected budget and fixed256MiB reserve are
archived as `storage_policy_v1` metadata. Reaching a limit fails explicitly
rather than deleting data or wrapping the archive. Rotation/retention must be supplied before indefinite deployment. Its
BOOTTIME timer does not wake a suspended CPU; HAL/RTC coordination is separate.

`--time-sync SUID` changes hub clock configuration; `--minute-read-clock SUID`
adds stock-style request time. `--minute-record-clock SUID` adds a freshly sampled
CLOCK_REALTIME timestamp to every continuous GET, with the same30s schedule,
readiness and journal limits as minute-record. All three require SSC_TIME_OFFSET_SECONDS, restricted
to whole-hour offsets. Source timestamps still need provenance validation.
`--sleep-trial SUID` enables tracking/detection and requires an independently
armed configuration-restoration supervisor before setting SSC_SLEEP_RESTORE_ARMED.
SSC_SLEEP_SECONDS is limited to1..180; SSC_MINUTE_SUID optionally adds minute
reads. These modes are not suitable for unattended permanent startup yet.

SIGTERM and SIGINT stop new requests and interrupt the idle BOOTTIME wait.
Signals are blocked before worker/vendor thread creation and consumed through
signalfd. After QMI release confirms callback quiescence, the queued archive is
drained and final status persisted; an interrupted minute transfer remains a
protocol failure even if its partial bytes are durably archived. A shutdown
marker records the signal number. Synchronous QMI calls retain their existing
timeouts; supervision must still bound a stuck vendor release or storage write.

A supervisor must invoke `--cleanup` after collector death to release its own
wake hold. The file lease prevents cleanup from releasing a live collector's
hold. This does not restore sensor configuration: sleep-trial restoration is a
separate supervisor responsibility. Never clear unrelated vendor wake locks.

Remaining integration: shared session lifecycle,
stream inventory/discovery automation, periodic processed collection, bounded
restart/retention, verified clock handling, and power-policy supervision. No
buffer ACK/delete is sent; retain this until transfer completeness and retention
semantics are established. See tasks0242–0252 for protocol and live evidence.

For `--minute-record`, optional NOTIFY_SOCKET enables systemd readiness. Use
Type=notify/NotifyAccess=main and a bounded startup timeout (the current check
runs after the first30s interval). READY requires at least one completed minute
transfer, no pending/protocol error, and accepted archive records matching the
durable count with no worker error/rejects. A failed configured notification
endpoint fails the capture; other modes reject NOTIFY_SOCKET rather than claim
readiness they do not implement. The status explicitly leaves freshness
unverified: a complete archived transfer can still contain historical data.

`--user-config SUID`, `--tracking-config SUID`, and `--detect-config SUID`
also export a private `config.json` containing boot/session identity, requested
mode, matching source/event ID and exact raw configuration payload. Export
requires exactly one matching reply, successful raw archive finalization and no
shutdown interruption; source/event collisions, malformed envelopes and payloads
over4096bytes fail. Publication uses exclusive creation and file/directory fsync
under a recorder wake hold. The payload remains schema-specific: consumers must
validate its fields before constructing a restoration plan. This is a baseline
snapshot primitive, not yet automatic activation or restoration. The old trial
scripts' hardcoded off-values must not become a permanent restore policy.

`--set-tracking SUID` and `--set-detect SUID` require SSC_CONFIG_VALUE exactly0
or1. `--set-permissions SUID` accepts SSC_RHR_PERMISSION and/or
SSC_SLEEP_PERMISSION exactly0 or1; omitted fields remain untouched. Mixed or
irrelevant value variables are rejected. All three require
SSC_CONFIG_RESTORE_ARMED=1: a caller contract, not proof of a working supervisor.
The caller must save and revalidate the baseline, durably record ownership and
restoration intent, and independently supervise restoration before invoking them.
Use a separate getter afterward; successful setter exit proves archive/transport
completion, not the requested configuration state.

Configuration setters, both legacy sleep-trial writes, explicit time-sync, and
timestamped GET requests
wait up to5s for their raw request and preceding archive records to reach fsync
before calling QMI. A failed or timed-out barrier prevents that send and poisons
the archive. Requests queued during the fsync remain asynchronous; normal sensor
callbacks never wait on the barrier. The writer syncs the barrier's written
prefix even if later arrivals keep the queue nonempty; it credits only records
actually written before that fsync and retains the wake hold for queued data.
Without a waiting barrier, queue-drain syncing remains unchanged.
Supervisor timeouts still bound a hung
storage syscall during teardown. A signal observed after the barrier also prevents
that mutation. A durable request is intent evidence, never proof the request was
sent/applied: recovery must read back firmware state. This does not replace the
executor's durable baseline/ownership or its responsibility to fsync the capture
parent directory before any operation that must survive a power loss.

Task0284 live-tested continuous timestamped GET alongside HAL recording. Four
complete transfers retained all12 prior frames and added two with current-window
time anchors (first retrieved about20–21s after those timestamps). Untimestamped
trials had generated anchors near the much earlier clock-sync time. This supports
using current-time GET requests for newly aligned anchors; it does not validate
sleep classifications or all per-sample timing. The stored timezone remains120
with a stock-style wire value2, so timezone interpretation is still unresolved.

Continuous minute modes optionally accept `SSC_SLEEP_OBSERVE_SUID`, selected from
current-boot discovery. On the same long-lived QMI client they send empty sleep
tracking/detection queries776/876 and archive all incoming indications, including
other source IDs. Readiness additionally requires one valid source-qualified reply
for each getter. This keeps a sleep-endpoint attachment open without writing its
configuration; it does not prove that the firmware will deliver future sleep-state
events to that client. Raw archives remain authoritative, and actual event delivery
must be validated separately. Do not use this option with one-shot modes.

`--rhr-read SUID` sends the stock resting-HR query ID1234 with an empty payload and
requires exactly one source-qualified1029 reply. Task0289 verified this read on
this firmware, retaining the float result in raw journal/private snapshot. Unlike
stock's shared encoder, it supplies no threshold values and sends no778/779 config.
A returned0 is not classified as a valid resting-HR measurement. Continuous polling,
onchange delivery and freshness remain separate integration/validation work.

Continuous minute modes also accept `SSC_RHR_SUID` from current-boot discovery.
They issue the verified empty1234 resting-HR query once per minute-GET cycle on
the same QMI client, archive replies in the same journal, and require a single
source-qualified1029 reply before the next cycle/readiness. No additional timer,
threshold write or onchange activation is introduced. Values remain raw; repeated
or zero replies do not establish fresh resting-HR measurements. The standard inventory includes fsl_rhr and fsl_wk, so normal native snapshots
can supply all current periodic processed-reader endpoints. Extended discovery
adds29 research names (47 total) without duplicating these standard entries.

`--workout-summary SUID` sends the stock empty779 summary query to fsl_wk and
requires one source-qualified779 reply. The reply is an opaque bytes field1;
raw data and the private snapshot are retained. It sends no778 workout/state/
permission command and no780 onchange configuration. Task0296 recovered this
request from stock get_wk_summary and its matching handle_summary_event.
The snapshot's4096-byte limit still applies; oversized/multipart replies cannot
be claimed as a complete snapshot. Empty results do not prove collection of an
actual workout, summary freshness, retention, or field interpretation.

Continuous minute modes optionally accept `SSC_WORKOUT_SUID`. Workout summary and
RHR reads share the minute-GET schedule, callback client and durable journal. Each
configured reader requires exactly one matching reply per cycle/readiness, without
an additional timer. Raw callbacks are always retained even when they are not the
expected reply. No workout is started by enabling this option.
# Offline heartbeat payload decoding

The SSC queue now has4096 message descriptors sharing a4MiB byte ring, plus one
65536byte in-flight payload. Either limit rejects input explicitly with ENOBUFS;
there is no eviction or silent drop. Copies use actual payload length rather than
65536bytes per event. The payload memory budget is unchanged from the previous
64 fixed slots; descriptor overhead increases modestly. queue_policy_v2 metadata
records both limits and worker size; final logs include byte high-water.

Task0298 exposed overflow with64 slots while most preserved indications were
139bytes. Host tests now cover512 small messages during blocked fsync,4609 records
across descriptor wrap, a split byte-ring payload, empty payloads, independent
byte-cap exhaustion, descriptor exhaustion and existing failure/wake races.
This provides finite burst headroom, not an unlimited-rate or overnight guarantee.

The journal worker now publishes `progress.json` after its first data fsync and
at most once per five minutes of CLOCK_BOOTTIME at later data fsyncs. No separate
timer wakes the CPU. A snapshot includes accepted/durable/rejected counts, queue
high-water, and `publication_boottime_ns`; it is a past observation, never a live
counter or proof the process remains healthy. No data means no new publication.
It uses phase `recording`, archive_complete=false and protocol_complete=null.
Use the capture's saved boot identity when interpreting its boottime.

Publication keeps the existing wake hold until data and snapshot are durable,
and does not hold the producer mutex across disk I/O. Snapshot errors fail the
worker conservatively. This adds a file and directory fsync per snapshot; actual
energy cost remains to be measured. `status.json` keeps its existing startup/final
contract, and remains authoritative for finalization. A final progress snapshot
can be stale by design. Task0306 is host-tested and ARM-built, not live-validated.

`python3 heartbeat_payload.py EVENT_ID PAYLOAD_HEX` decodes saved heartbeat
payloads only; it sends no SSC request. Supported IDs are767 control,1028 data
and1029 event-config. It preserves full-width uint64 timestamps, uint32 PPG and
quality, unknown field bytes and the original payload. Missing required fields
are reported because the original vendor serializer permits partial messages;
malformed, duplicate known fields and overflow are rejected.

The stock handler's nonzero force_stop marker in a767 reply is exposed separately
from transfer completeness, source identity and freshness, none of which a payload
alone can verify. Timestamp units and physiological meaning remain unspecified.
Tests: `python3 -m unittest discover -s tests -p test_heartbeat_payload.py`.


Diagnostic getters `--chrm-config SUID` and `--tracker-config SUID` use the
stock-code-derived requests from task0315. CHRM sends empty message775 and accepts
exactly one matching source/event775 reply. Tracker sends message768 with an
explicit READ_TRACKER request, zero state and empty required payload; its expected
reply is1029. Both reuse the existing bounded snapshot path: raw requests/replies
are journaled, QMI is released, the archive must finalize successfully, and only
then can private config.json be published. Missing, duplicate, oversized or
wrong-source replies cannot establish a snapshot. Payload bytes are preserved;
no physiological meaning or complete-setting schema validation is claimed.

These new getters have host tests and ARM compilation only until a live trial is
recorded. They are not enabled in continuous recording, configuration activation,
or default snapshots. Use a current-boot discovered SUID, an independent bounded
supervisor and a fresh private journal directory. Do not run against a collector
that already owns the global lease. CHRM's mode-only reply is insufficient for
restoring all optional request settings. Tracker config states are not health
measurements. No CHRM/SpO2/RHR activation or tracker-control CLI was added.
