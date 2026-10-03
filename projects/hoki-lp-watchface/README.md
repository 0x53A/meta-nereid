# Hoki low-power watchfaces

The managed renderer supplies four selectable Sidekick faces for the opt-in
[everyday sleep system](../hoki-powerd/SLEEP.md). The known digital clock sequence
comes from the physically verified task0174/0182 pilot. New compositions and the
integrated handoff have not been tested on the watch. The datetime resource now
applies the system's current local UTC offset, including DST, on each upload;
on-watch timezone verification remains pending. An uninterrupted ambient session
crossing a timezone/DST change keeps its uploaded offset until the next handoff.
No main-CPU redraw loop runs in ambient mode.

| Manifest | Scene | API coverage |
|---|---|---|
| hoki-digital | Pale HHmm, opaque backing | Unicode custom font, datetime, z order |
| hoki-seconds | Mint HHmmss | Proportional font, autonomous seconds |
| hoki-orbit | Mint HHmm with orbit dial/hand | Alpha bitmap, clock rotation |
| hoki-instrument | Amber HHmm, chevrons and two counters | X/Y/diagonal flips, blink, legacy font/numeric and colored numeric |

Counters are autonomous 0–59 clock-derived examples, **not sensor readings**.
Rotation, flips, blink and proportional/numeric calls had accepted-API evidence;
that does not establish their visual result. Unsupported FPS/colored-string and
unadvertised scaling features are excluded. Resource delete/replace was unstable
in prior trials and is not used. There is no arbitrary Sidekick code loader.
Ambient dim/normal brightness is explicit; ALS is disabled by the established
pilot sequence. TWM flags are populated, but no TWM service or metric subscription
is enabled by these faces.

Install strict version-1 JSON manifests under `/usr/share/hoki/ambient-faces/`.
IDs permit letters, digits, hyphens and underscores. Fields are `version`, `kind`,
`name`, opaque ARGB `foreground`, `brightness` and `dim_brightness` (0–255, dim no
higher than normal). Kinds are the four versioned renderers in `src/bundle.rs`.
The Sidekick manifest selects bounded built-in assets. Its `placeholder` field
declares a Wayland companion as an argv array, for example
`["/usr/lib/hoki-lp-placeholder", "hoki-digital"]`. All four supplied faces use
the Slint companion displaying the face name, **Uploading…**, and **Please wait**.
The compositor runs it as ceres without a shell, separately from the primary
watchface. A future face can register a matching Wayland design through the same
field. Legacy manifests without the field use the standard companion.

Before starting a Sidekick upload, the compositor waits for the companion's
buffer to be submitted and acknowledged by the display proxy. That submitted
frame stays on the panel during the synchronous upload; there is no artificial
minimum display time or progress percentage. Startup is bounded to three seconds
before interactive fallback. Navigation and crown gestures can cancel the wait.
The companion stays alive between handoffs and has no animation/timer loop.
Install the updated renderer, compositor, companion and manifests together: older
strict manifest readers do not recognize `placeholder`.

Screen/color/operation capabilities and a minimum free-memory guard
are checked before upload; actual encoded resource acceptance is checked through
HAL results. RLE/compression means this is not a proven peak-memory estimate.

`tools/generate-scene-assets.py` reproducibly creates the original geometric
PNG assets in `assets/` using Python's standard library. The existing digit/font
and black-backing inputs are retained beside the renderer. Small alpha assets,
solid backings and resource reuse keep the scenes bounded; no external artwork
or runtime network fetch is required.

Only HWC proxy calls `managed prepare ID`, `managed enter`, `managed exit`, as
ceres. These operations have an eight-second watchdog and never initialize HWC.
The proxy serializes upload, HWC transition and display entry/exit, with recovery
markers and timeouts. Do not invoke managed operations alongside an active owner.
Normal applications remain alive during managed transitions.

The older standalone pilot commands below remain bounded research tools and must
not compete with the managed display owner. Their historical service-restart
orchestration closes UI apps; the managed path does not.

Build from this directory using the repository's nix-shell cargo cross-build and required patchelf steps. Binaries: hoki-lp-watchface, hoki-suspend-check. Display binary runs as ceres; suspend helper/root orchestration runs as root. Hardware libraries are loaded dynamically.

The custom Hoki layer now includes these two binaries under `/usr/lib` so they
survive image replacement. Automatic sleep remains off by default; the integrated coordinator and manifests
are packaged alongside these binaries. The historical task-specific orchestration below
still requires its explicit recovery owner; packaging alone does not authorize
running the display handoff without that recovery arrangement.

`hoki-lp-watchface preflight 1`: query limits only.
`hoki-lp-watchface face 70`: upload/commit font and backing, give BG display ownership, wait for timeout/SIGTERM, release.
`hoki-lp-watchface release 1`: bounded recovery API (must run after display client exits).
`hoki-suspend-check awake 3`: verify alarmtimer expiration without suspend.
`hoki-suspend-check mem 25`: arm kernel-managed wake alarm, use wakeup_count protocol, attempt suspend, report elapsed minus awake clocks and alarm expiration. Any early wake ends test; no repeated suspend loop.

Deployed scripts in /tmp are task-specific and intentionally not boot-enabled. Outer unit MUST have ExecStopPost=/tmp/hoki-lp-recover-0182.sh, RuntimeMaxSec=90 and TimeoutStopSec=5. Never run trial.sh directly without that recovery owner.

Limits and transition/health-logging requirements: ../_Tasks/0182_Low_Power_Watchface/design.md. Test results: corresponding summary.md. Active graphics service must not be restarted for probing. Resource-allocation failure-boundary tests are deliberately excluded after earlier vendor instability.
