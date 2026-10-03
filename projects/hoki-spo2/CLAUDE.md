# SpO₂ app integration

Follow the repository CLAUDE.md for builds and deployment. The custom layer is
`meta-nereid`; the app requires its `0006-spo2-reading-contract.patch` in both
sensorfw and sensorfw-hybris-binder-plugins. Rebuild and deploy those components
together: their internal `Spo2Data` layout changed even though the legacy D-Bus
`spo2` property and socket record layout were retained. Do not hot-mix old and
new daemon/plugin libraries.

The additive `local.Spo2Sensor.spo2Reading` property has D-Bus signature
`(tdddd)`: timestamp in microseconds, oxygen, confidence, algorithm code, signal
code. It preserves all four vendor float fields as doubles. Algorithm 5 is FINAL;
signal 0 is clear. These codes are not accuracy tiers or bit flags.

`src/measurement.rs` owns the pure acceptance rule. It follows the recovered
stock UI lower thresholds (truncated oxygen >80, confidence >=80), plus finite
values and percentage upper bounds of 100. A progress report or rejected final
never becomes a result on timeout. This is software policy, not clinical sensor
validation.

Each request uses a pre-start cached timestamp and CLOCK_BOOTTIME freshness
floor, assumes the HAL's Android elapsedRealtime timestamp domain, and rejects
implausibly future reports. Clock agreement still needs live validation. The
180-second timeout is measured with Instant; successful readings in long archived
captures do not prove a result will arrive within this window.

Keep the sensor socket connected and drained throughout the session. Start/stop
belong to `local.Spo2Sensor`. The serial worker owns RAII cleanup; generation
checks prevent old queued UI updates from changing a newer request. Stop may
queue a new request while cleanup finishes. D-Bus calls have bounded timeouts.

Validation: `nix-shell --run 'cargo test --offline'`, then the standard ARM release
build. Task `_Tasks/20260925_Spo2_Final_Results` records reference-capture replay,
Qt checks and outstanding hardware validation. Detailed failures go to stderr;
watch error messages stay short enough for the circular screen.

## Broker client (deployed 2026-09-28)

The activity integration replaces this app's direct sensorfw D-Bus/socket session
with a connection-scoped `spo2` request to the health-policy daemon. It polls
preserved raw timestamp/oxygen/confidence/algorithm/signal fields from durable
HAL checkpoints. The same pure acceptance and CLOCK_BOOTTIME freshness checks
apply; there is no direct-HAL fallback. Opening a measurement during a running
activity reports busy. Deploy with the new controller/policy and their units.
Historical D-Bus layout guidance above remains relevant to other sensorfw clients.
Native tests may use `nix-shell --arg nativeOnly true --run 'cargo test --offline'`.
Live broker conflict handling was verified during a running lease. Successful
on-wrist readings through this migrated client still need validation.
