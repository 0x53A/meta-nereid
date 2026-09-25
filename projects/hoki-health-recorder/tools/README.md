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
