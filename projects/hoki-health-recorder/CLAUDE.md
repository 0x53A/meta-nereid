# Recorder development and current manual service

Follow the containing layer and workspace CLAUDE.md. Build from this directory
with its shell.nix; the recorder bundle builder is ../../build-health-recorder.sh.
Deployment belongs to the session explicitly authorized to perform it.

The manual Settings service is separate from the older combined HAL/SSC
research orchestration documented in deploy/README.md. Its implementation is
deploy/recording-session.py, deploy/hoki-health-recording.service and
deploy/30-hoki-health-recording.rules. It records HAL events and battery samples.
The native controller reports checkpoint readiness and its next BOOTTIME deadline
to hoki-powerd; powerd decides whether the configured sleep policy may run. The
service does not configure SSC or change radios.

Ordinary manual captures retain immediate HAL delivery. The full-profile
buffering experiment is a separate explicit service-manager environment opt-in:
`HOKI_BUFFERED_FULL_TRIAL=1`, optional latency step `7`, `20` or `40` seconds
(default `7`), and optional duration `1..1800` seconds (default `300`). Its
fallback flush/deadline is ten seconds beyond the requested latency. FIFO counts
classify the probe but do not clamp it; missing live FIFO fields are recorded as
unknown. This is an exploratory, finite trial that may expose data loss, not a
safe default. The manual unit accepts these values through `PassEnvironment`;
the Settings toggle does not set them.

`HOKI_BUFFERED_TRIAL_SELECTION=continuous-only` optionally restricts that finite
trial to continuous channels while retaining their full-profile rates. Default
`full` retains all channels. Reduced trials explicitly record their selection and
do not provide full-profile coverage; they isolate immediate derived deliveries.
The historical `buffered_full_trial` field denotes trial machinery being enabled,
so inspect `buffered_trial_selection` and the actual selected descriptors too.

Narrow isolation choices use the same opt-in and bounds: `ppg-motion` selects
types 1/4/65572; `ppg-motion-hr` additionally selects type 21 and
`ppg-motion-spo2` additionally selects type 65561. Required types must be present.
Shared demands remain identical, added metrics immediate. These choices test
recorder-demand interactions; they do not assert hardware sampling stops.

Settings starts/stops hoki-health-recording.service. The root helper supervises
the existing native controller; the actual capture queue and storage worker
live inside sensorfw's patched binder backend. The controller communicates via
a private Unix socket. Multiple app sensor requests are arbitrated, but the
recording backend currently allows only one capture per sensorfw lifetime.
Consequently service preparation and cleanup restart sensorfwd, potentially
interrupting other sensor clients. Reusable in-process sessions are not implemented.

Archives are private and uniquely named under /var/lib/hoki-health-recordings.
Runtime ownership is under /run/hoki-health-recording. The service creates only
its owned runtime sensorfwd override and refuses foreign recording configuration.
Stopping must finalize or retain failure evidence, then restore sensorfw.
Existing archives are never deleted and interrupted captures do not resume on boot.

Ordinary HAL captures stream gzip directly in the storage worker and have no
per-capture byte ceiling. Admission requires250MiB free reserve plus16MiB
headroom; the supervisor asks for a clean stop at reserve+8MiB, and the backend
checks the reserve on writes. Battery<=15% when not charging still stops capture.
Concurrent filesystem writers can race the check. Gzip checkpoints finish a
member, sync data, then publish compressed/logical boundaries atomically; recovery
reads only the committed prefix. Old uncompressed captures remain supported.


Local lifecycle tests and runtime-package validation passed on 2026-09-25.
This session did not deploy or validate live start/stop. Before relying on the
service, the deployment session should verify two start/stop cycles, readable
distinct archives, ceres permission handling and restored sensorfw configuration.
Preserve ongoing captures before any disruptive action.

Ordinary full collection defaults to periodic SpO2 demands: at-most-180-second
attempts on a fifteen-minute start cadence regardless of outcome, with early
release after a fresh stock-rule accepted FINAL. Daily/sleep/activity continue to exclude SpO2.
`HOKI_SPO2_POLICY=continuous|off` overrides ordinary full behavior. Buffered trials
retain continuous demand and reject other policies. Metadata marks periodic full
coverage incomplete, and logs acknowledged transitions. The supervisor supplies
shared private attempt-start state so cadence survives service restarts.
Acceptance is a software-quality rule, not medical validation; preserve raw values
and flags. Existing checkpoint work may delay demand release. See README.

## Shared consumer implementation (deployed 2026-09-28)

For ordinary operation, the older manual-session lifecycle above is superseded
by `manual-consumer.py`, `health_broker.py`, and `src/consumer_broker.rs`.
Settings manual recording, Settings automatic profile, running and the SpO2 app
share one profile-owned capture. Requests change demands without restarting
sensorfw. First/last-consumer setup/cleanup retains the old restart limitation;
explicit buffered trials retain the isolated research path. Read README's shared
consumer protocol before changes. Deploy controller, policy scripts, units and
SpO2 client together. Preserve a running capture before that migration.

Live running/full lease union and release retained one capture; conflicting
immediate SpO2 reported busy. Activity touch start/pause/resume/stop and complete
finalization passed. Dynamic broker capture remains CPU-inhibited and bypasses
the fixed-plan suspend-readiness check while validating owned backend health.

Full subscriptions now default to each continuous descriptor's advertised maximum
rate (hoki: 50 Hz motion/compass/orientation, 25 Hz pressure, approximately 26 Hz
PPG); this includes explicit buffered full trials. Missing maxima retain bounded
defaults and event channels remain event-driven. Rate overrides and applied/observed
status cover all selected continuous types. Faster full rates reduce FIFO time
coverage; the existing trial bounds and optical exclusivity still apply.

Shared Full buffering is now available via `HOKI_SHARED_FULL_BUFFERED=1` on the
profile recording service, default off. Full-only consumers use 7-second
continuous-channel latency and at-most-8-second fallback maintenance with the
existing descriptor/durability gates. Other profiles restore immediate delivery.
See README for delayed subscription acknowledgement and the live-validation
limits; do not equate wakeup descriptors with demonstrated deep-sleep endurance.
