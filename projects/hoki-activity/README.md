# Hoki activities

Running PoC: select Running → acquire sensors/GPS → Start (fix optional) →
Pause/Resume → Stop → summary and relative track. Screen pages show metrics or
track. GPS and sensor recording continue during preparation and pauses. Closing
the UI does not stop the activity: the user service owns its lifetime.

The UI uses Slint with the software Wayland renderer. A Rust/zbus client talks
directly to the existing GeoClue Hybris provider; the UI and bridge load no Qt.
The existing system provider is still built with Qt. Removing Qt system-wide
requires a separate provider migration; this app does not replace that service.
Raw GPS calls, snapshots, signals and errors are embedded in the activity journal.
Provider loss ends GPS collection with a visible error while health collection
continues; starting a new activity reconnects. Automatic GPS recovery is future work.

## Collection contract

The health-policy daemon owns a connection-scoped request registry. Settings is
one consumer; activity and SpO2 are others. The native recorder applies the union
in its existing capture, selecting the fastest requested period for each channel.
Changing requests never restarts sensorfw. Initial capture setup still has the
legacy sensorfw restart; that limitation is visible during preparation.

Running requests accelerometer/gyro at 50 Hz, steps, HR, heartbeat/intervals and
periodic SpO2. Full Settings collection remains a superset. SpO2 windows explicitly
release heartbeat/interval demand first; requests and acknowledgements are logged.
This is a conservative optical scheduling policy, not proof of strict electrical
mutual exclusion. Immediate SpO2 requests during a running session return busy.
App clients must use this broker; external/legacy direct sensorfw clients remain
outside its optical policy. Sensorfw still arbitrates their underlying demands.

A logical activity has its own UUID and append-only JSONL journal with BOOTTIME,
UTC, boot identity, raw GPS events, capture references and lifecycle markers.
Preparation, start, pause, resume and stop are durable before acknowledging UI
commands. Raw records are never trimmed or rewritten. Active time excludes pauses;
elapsed time includes them. Distance only joins accepted fixes within an active
segment; no straight line is added across pauses, stale fixes or GPS gaps.
Pace is provisional GPS-derived pace; no distance is invented before GPS fixes.
GPS lock means a recent valid signal, never a cached snapshot or satellite count.

Private local data only. The exporter produces segmented GPX and CSV; originals
remain available for later trimming, re-segmentation and improved processing.
Interrupted sessions are retained and labelled interrupted, not silently resumed.

## Analysis direction

- GoldenCheetah: desktop training analysis, heart-rate/pace/power zones:
  https://www.goldencheetah.org/
- FitTrackee: self-hosted activity history, maps and charts:
  https://github.com/SamR1/FitTrackee
- gpx.studio: crop and split GPX tracks:
  https://gpx.studio/help/toolbar/scissors

These process activity-level samples, not Hoki's packed optical data. Export is
our adapter boundary. Later work: editable logical ranges, configured HR zones,
laps, richer charts and phone transfer. Running power requires an explicit model,
body mass and reliable speed/grade; it is not a measured output of this PoC.

## Power and validation limits

The PoC holds a CPU inhibitor while prepared, running or paused to preserve GPS
continuity. It does not hold the display on. This costs battery and is deliberate
until suspend-safe GNSS delivery has been measured. Stop/cancel releases it.
One-second durable checkpoints also favour capture integrity over battery life.
Running starts without GPS but waits for an acknowledged sensor recording.

GPS distance requires valid fresh position signals with detailed horizontal
accuracy 0–35 m, increasing source time, gaps <=10 s and implied speed <=12 m/s.
Rejected positions remain in raw GPS logs. These are provisional running filters,
not a calibrated distance guarantee. Pace is point-to-point and can be noisy.
GPX exports retain separate segments; CSV and GPX attach recent HR where available.
