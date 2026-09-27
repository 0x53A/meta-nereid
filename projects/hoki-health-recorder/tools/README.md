# Saved raw-capture verification

Run `python3 tools/verify_hal.py SAVED_HAL_DIRECTORY` from the recorder project.
The verifier streams each HOKISEN1 segment, checks its header/durable length and
checkpoint totals, and hashes the entire file including unacknowledged tails.
It reports extra segments separately. Memory use is bounded per channel (maximum
4096 channels) and per read block; it retains no per-event timestamp arrays.
File summaries grow with the number of segments.

A successful exit proves that the checkpointed byte prefix is structurally
readable, not that the session completed or the hardware delivered every sample.
`checkpoint_claim_and_bytes_consistent` additionally requires final/complete
checkpoint flags, zero reported producer losses, no tails and no extra segments.
Controller/flush completion must be checked against controller.json separately.
Compare returned SHA256 values with independently preserved source checksums;
HOKISEN1 does not contain per-record CRCs. A self-computed hash alone does not prove
that file contents match the original watch capture.

New sensorfw captures add a booted rootfs reference and selected OverlayFS
spot checks to `hal/session`. The rootfs hash comes from the installed bundle
manifest; it does not hash the full image during capture. Selected visible files
are compared with the read-only `/.hoki-lower` view, and differences are
identified separately. The checked vendor HAL libraries are in the rootfs.
ADSP, BG and modem images under the separately mounted, read-only `/firmware`
partition are outside this rootfs reference, as are other mutable files.
Older `hal/session` files have no such reference; later filesystem snapshots
cannot fill that gap.
For a checked file to inherit the bundle's identity, require `rootfs.status`
to be `ok`, `rootfs.overlay.lower_mount.status` to be `backing_image_match`,
and that file's `relation` to be `same_inode_as_lower`.

Timestamp statistics are raw differences within each handle/type channel. They
do not establish a shared clock domain, freshness or physiological accuracy.
Metadata events (type0) are excluded from interval/age statistics. Mean/minimum/
maximum intervals are reported; an exact median is deliberately not computed
because that would require retaining intervals or an additional disk-based pass.

Run `python3 tools/hal_coverage.py SAVED_HAL_DIRECTORY` to join these statistics
with controller selection and activation metadata. The report includes selected
channels with zero records, separates type0 metadata, and lists unexpected
handle/type pairs. It reports HAL reporting modes so silent on-change/trigger
channels are not mistaken for periodic sample loss. Silence in a continuous
channel requires investigation; optical suppression on charger is one possible
cause. Neither output nor activation establishes freshness. Metadata-to-event
provenance and controller completion still require independent checks.

Tests: `python3 -m unittest discover -s tools -p 'test_*.py' -v`.

## Staged buffered power trials

For captures whose supervisor battery JSONL has BOOTTIME but no MONOTONIC,
assemble `measurement.json` from the recorder's existing paired checkpoint
samples and the nearest existing battery rows:

```sh
python3 tools/assemble_buffered_measurement.py PATH_TO_SESSION \
  --output measurement.json --context-json PATH_TO_CONTEXT.json
```

`--context-json` is optional. When supplied, it is a JSON object with a
`context` object (`display_state`, `wifi_up`, `bluetooth_powered`, `usb_state`)
and optional `powerd_status_start` / `powerd_status_end` objects. The assembler
uses the first and last paired checkpoint samples inside the controller
interval for the suspend clock window. It chooses the nearest battery JSONL
rows for counter endpoints and preserves their separate BOOTTIME values and
signed skew. This is a checkpoint window, not a whole-controller measurement;
the tool does not invent MONOTONIC values for the battery rows. Optional
powerd status snapshots remain separately labeled and do not inherit a
checkpoint timestamp. It refuses to overwrite an existing output. Include the
resulting `measurement.json` in the host-side checksum manifest.

Run the analyzer on frozen host-side session directories:

```sh
python3 tools/analyze_buffered_trial.py --max-latency-ms 40000 \
  --manifest PATH TRIAL...
```

A trial
directory contains `session.json`, `battery.jsonl`, `measurement.json`,
`hal/controller.json`, `hal/checkpoint.json`, and the durable event segments.
The watch-produced checksum manifest covers the captured source files;
`trial-sha256.txt` is a separate host manifest covering the complete analysis
inputs, including endpoint measurements and any retained journals/snapshots.

The analyzer audits one trial or an ordered set of bounded latency rungs. It
re-verifies source hashes and durable HAL counts, reports producer-loss counters
and nominal-period accel/gyro gap estimates, and reports PPG mode/gap
associations. Sensor gaps do not invalidate a structurally sound trial. The
assigned supervisor battery endpoint rows feed the charge-counter slope over
their own BOOTTIME interval; they do not provide paired suspend clocks. Other
off-checkpoint battery rows remain visible as possible extra-timer confounds.
Paired BOOTTIME/MONOTONIC samples estimate suspend time only over their labeled
clock scope. The report also checks read-only powerd boundary status: its
`max_sleep_seconds` can wake sooner than the recorder fallback and clip the
20/40 s rung. To classify powerd residency and kernel deep/s2idle entries, save
both full-boot journals in `short-monotonic` format; `wakeup-before.txt` and
`wakeup-after.txt` provide optional blocker deltas. Checkpoint `wake_held`
samples are reported only if the recorder captured them on its existing poll
path. The tool never contacts or changes the watch.

See [`_Tasks/20260926_Matched_Power_Test`](../../../../_Tasks/20260926_Matched_Power_Test/summary.md)
for admission, stop/recovery gates, the staged protocol, and interpretation
limits.

## Saved recording power evidence

Run `python3 recording_power.py SAVED_TRIAL_ROOT` against a stable saved trial
containing `recording/hal/controller.json`, any `suspend-*-{intent,result}.json`
files, and optional `telemetry.csv`. It rejects foreign boot/session identities,
orphan/duplicate evidence, inconsistent clock measurements and invalid telemetry.
Duplicate JSON field names are rejected rather than silently replacing metadata.
Non-finite numeric constants (`NaN`, `Infinity`) and floating-point overflow
are rejected during decoding, including in otherwise unexamined nested fields.
The same decoder protects HAL checkpoints and coverage controller metadata,
including nested sensor fields; each metadata document must be an object.
An intent without a result remains uncertain; it is not proof of suspend entry
or failure. Results summarize BOOTTIME minus MONOTONIC only within returned
`mem` calls, excluding gaps between attempts. Alarm expiry does not establish
that the alarm caused the wake. Optional start/end BOOTTIME measurement fields
must appear together, start no earlier than intent, and agree with elapsed time.
Legacy duration-only results remain accepted; intent time is not a call start.
No whole-session suspend percentage is inferred.
Explicit failed `mem` writes are counted separately with their errno, without
claiming a wake cause, failing driver, or time asleep. Pending-durability skips
are also separate from measured returns. Neither is a missing result.
Failed wakeup_count commits are recorded separately with suspend_requested=false;
they contribute no sleep time. Optional retryable metadata must match the narrow
stage/errno policy (EINVAL at counter commit, EBUSY at mem write).
Battery output reports sampled percentage change and gaps, without converting
sparse samples to energy or extrapolating battery life. Empty display status is
unknown. Telemetry CSV requires unique nonempty column names, including
`boottime`, `capacity`, and `status`, and valid CSV quoting. A valid header with
no rows reports zero samples; a missing telemetry file reports no battery data.
Malformed or incomplete input is rejected. Use source checksums, HAL verification/coverage, SSC archive/protocol
checks and restoration/lifetime evidence separately; this report cannot certify
a complete recording or fresh physiological measurements.

Tests: `python3 -m unittest discover -s . -p test_recording_power.py`.


The HAL verifier retains up to eight largest positive source timestamp intervals
per non-metadata channel. Each includes previous/current source and arrival times
and zero-based global durable-record indices, including across segment boundaries.
These are interval examples, not an exhaustive gap list or a dropped-sample count.
Coverage reports maximum interval/requested-period only for continuous channels
with a positive requested period. On-change/trigger timing, unspecified periods,
clock-domain assumptions and rate guarantees need separate interpretation. A
clean journal still does not establish completeness before the HAL delivered data.

## Recovered health fields

`python3 tools/health_decode.py SAVED_HAL_DIRECTORY` produces an offline JSON
summary for SpO₂ (type65561) and beat interval (type65574). Optional
`--source-window-ns START END` separates reports outside an explicitly supplied,
inclusive source-timestamp window; it does not certify a common clock or freshness.
The verifier checks durable prefixes first; the decoder rechecks source hashes
while reading. Unacknowledged tail records are never interpreted. Compare the
reported hashes against independently preserved capture manifests.

SpO₂ fields are value, confidence, algorithm state and signal state. RUNNING
estimates are provisional; FINAL alone is not acceptance. Stock service rules
truncate value/confidence to integers and require value >=80, confidence >=80,
and signal0; stock UI additionally requires value >80. The decoder conservatively
rejects nonfinite fields and fractional state codes instead of reproducing Java's
casts for malformed data. Unknown state codes remain unknown, not bit masks.
These are software eligibility rules, not clinical validation or independent
measurement counts. The pure `decode_health` function retains all64 original
payload bytes as hex, including nonfinite bit patterns; the aggregate report
keeps source hashes rather than copying every payload.

RR is labelled **beat interval (ms)** for this firmware path, supported by
18,465 overnight comparisons with heartbeat timestamps. Packed PPG remains
undecoded. See [task0490](../../../../_Tasks/0490_Health_RR_SpO2_Trace/summary.md) for
stock-code and retained-capture evidence. No watch connection, activation,
sensor configuration, or service change is performed by this tool.
