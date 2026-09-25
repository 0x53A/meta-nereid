# GeoClue recorder in the Hoki GPS test app

Hoki's overlay adds an explicit Start recording / Stop & save UI to `asteroid-gps-test`. Upstream app repositories remain unchanged. Recording is opt-in; opening the app alone does not start GPS.

The recorder subscribes directly to the **GeoClue 0 provider's session D-Bus interfaces**. This bypasses QtPositioning's lossy conversion and startup position cache while exercising the same GeoClue provider and LocationAPI backend. It does not capture proprietary modem diagnostics or fields that GeoClue never exposes.

## What is saved

Each session creates a new file under the ceres user's `$XDG_DATA_HOME/gps-recordings` (normally `~/.local/share/gps-recordings`). Directory permissions are 0700 and files 0600. The recorder doesn't print coordinates to the journal.

JSONL schema 1: one complete JSON object per line, with `sequence`, receive UTC (`received_utc`), `boottime_ms` and session-relative `elapsed_ms`. CLOCK_BOOTTIME includes time spent suspended; UTC can change when clock synchronization occurs. The native GeoClue timestamp remains separately in the arguments.

- Raw `PositionChanged`, `VelocityChanged`, `SatelliteChanged`, `StatusChanged` signals with sender, interface, member, D-Bus signature and **all arguments in their original order**.
- Initial `GetProviderInfo`, `GetStatus`, `GetPosition`, `GetVelocity`, `GetSatellite`, `GetLastSatellite` replies. `source: snapshot` distinguishes these from `source: signal`; method calls are separately logged, including requested options. Replies can interleave with signals and are recorded in receive order. `applied_to_ui: false` marks initial replies superseded by newer signals (and previous-satellite snapshots); their full data is still retained.
- Reference acquisition, requested update interval (1000 ms), provider owner changes/reacquisition, D-Bus errors, session start/end and reason.
- Ten-second heartbeat with signal/fresh-fix ages to expose quiet intervals. A suspended process cannot write events; the BOOTTIME gap after resume records elapsed time honestly.

Argument layouts:

| Interface/member | Arguments |
| --- | --- |
| Position | `[fields, timestamp_s, latitude_deg, longitude_deg, altitude_m, [accuracy_level, horizontal_m, vertical_m]]` |
| Velocity | `[fields, timestamp_s, speed_knots, direction_deg, climb_m_per_s]` |
| Satellite | `[timestamp_s, used_count, visible_count, [used_prn...], [[prn, elevation_deg, azimuth_deg, snr]...]]` |
| Status | `[status]`: 0 error, 1 unavailable, 2 acquiring, 3 available |
| Provider info | `[name, description]` |

Positions and velocity retain validity bitmasks (position: latitude=1, longitude=2, altitude=4; velocity: speed=1, direction=2, climb=4). NaN/infinities are represented as explicit strings `NaN`, `+Infinity`, `-Infinity`, because JSON cannot represent them numerically. Zero signal levels, empty lists, invalid fixes and duplicate reports are retained. Satellite constellation is not a separate GC0 field; retain its PRN encoding rather than inventing one. The layer's provider reports speed in knots; it is not silently converted to metres per second in the log.

`fresh_for_session` is derived metadata, never a filter: true only for a valid position **signal**, at most ten seconds old, no older than elapsed BOOTTIME plus one second (GeoClue timestamp resolution), and no more than one second in the future. This avoids a fixed wall-clock start becoming invalid after clock synchronization. Initial snapshots are never labelled fresh. Every position is still recorded regardless of this flag. The UI shows age of the last fresh fix instead of calling a cached snapshot a fix. Stop freezes elapsed time and fix age; the latter is labelled as the final fix age.

## Lifetime and storage

Recording owns a separate D-Bus connection/reference, independent of other GPS apps. Stop closes that connection, which lets the provider release exactly this client. App exit/disconnection also releases it. Provider restarts and errors are recorded. After owner loss, the recorder attempts D-Bus reactivation after 2, 4 and 8 seconds, reacquiring its reference and snapshots. A provider that returns independently cancels the pending delay. After three failed recovery attempts it stops with an explicit `recovery_exhausted` end marker and visible error. Initial acquisition/options failures also stop with an error. Gaps remain in the log; no data is invented.

Each line is flushed to the OS, not fsynced individually. A crash/power loss may leave a missing end marker or incomplete final line; readers must tolerate that. The last writes are not guaranteed durable across sudden power loss. Write failures stop recording and show an error; no further end marker is attempted after a failed/partial write, preserving the parseable prefix and at most one torn final row. A session stops at approximately 128 MiB (with reserved space for an end marker). Existing recordings are never overwritten or automatically deleted.

The app does not stop recording because its display becomes inactive and takes no display or system wake lock. Sustained recording through actual system suspend still needs outdoor/suspend validation; missing deliveries appear as gaps. Stop recording before closing the app when possible. The file can be copied over SSH; no upload is performed.

## Build and checks

Sources live here; `meta-nereid/recipes-asteroid/asteroid-apps/asteroid-gps-test_%.bbappend` installs them into the upstream app at build time and registers the QML type with a small patch. The normal image wrapper syncs this directory. BitBake builds the app, so no Rust/patchelf build procedure applies.

Task `_Tasks/0157_GeoClue_Recorder` holds build and validation evidence. `tests/recorder-test.cpp` runs a synthetic provider on an isolated D-Bus session: nested satellite decoding, nonfinite values, invalid/cached/live positions, owner restart, errors, stop/restart and private storage. Its `--unavailable` option verifies bounded failure recovery. Its `--live [seconds]` option performs a bounded real-provider capture (20 seconds by default, maximum 120); it must run as ceres on the normal user bus. Synthetic test data never reaches that bus.

Pre-walk recovery and abrupt-exit validation: `_Tasks/0159_GPS_Pre_Walk/`.

Use `python3 hoki-gps-recorder/summarize.py recording.jsonl` for a coordinate-free summary of event counts, satellite maxima, freshness, gaps and completion. It accepts multiple files and tolerates a torn final line after an interrupted write, including incomplete UTF-8. Satellite-report gaps include quiet time before the first report and after the last report.

Satellite maxima describe live `SatelliteChanged` reports; initial snapshots remain in event counts but do not contribute to those maxima. `complete` means the last parsed event is a session-end marker and no torn final row follows it. Check `sequence_discontinuity` separately: an end marker does not prove that all preceding records are present. Fresh-fix counts use the recorder's `fresh_for_session` metadata, without reinterpreting coordinates or timestamps.

The summary rejects noninteger or negative clock values, regressing boottime or elapsed time, and nonboolean freshness markers. Equal millisecond timestamps are valid. Sequence values must be positive integers; discontinuities remain separately reported. A syntactically valid but malformed record is not treated as a torn tail.

Live satellite statistics require five arguments, nonnegative integer used/listed counts, and four-element satellite tuples with integer signal strength. Malformed statistics fail validation; reported counts are not recomputed from list lengths.

Run the offline summary regressions with `python3 -m unittest discover -s hoki-gps-recorder/tests -p 'test_*.py' -v`. They use synthetic temporary recordings and do not access the GPS provider.

Whole-stack review and expanded ARM regression evidence: [`_Tasks/0160_GPS_Stack_Review`](../../../_Tasks/0160_GPS_Stack_Review/summary.md). Coverage includes early status, delayed snapshots, real partial writes, the file cap during owner loss, queued events across sessions, bounded recovery and final age freezing.
