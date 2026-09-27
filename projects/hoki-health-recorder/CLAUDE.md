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

HAL budget: min(1 GiB, available space - 256 MiB reserve - 16 MiB headroom),
rounded down to MiB; reject below 128 MiB. Monitor requests stop 8 MiB before
that limit and at battery <=15% when not charging. Concurrent filesystem users
can still consume space. Do not apply the combined SSC admission formula to this
HAL-only service or equate a byte budget with guaranteed recording duration.

Local lifecycle tests and runtime-package validation passed on 2026-09-25.
This session did not deploy or validate live start/stop. Before relying on the
service, the deployment session should verify two start/stop cycles, readable
distinct archives, ceres permission handling and restored sensorfw configuration.
Preserve ongoing captures before any disruptive action.
