# Hoki low-power watchface pilot

Standalone bounded BG face, derived from the physically verified task0174 font/backing sequence. Black background, pale HHmm digits, no AP redraw loop. Vendor time currently needs local-time verification. Normal UI must be stopped before `face`; the orchestration scripts enforce exclusive ownership and restore normal UI afterward.

This is a deployed **pilot**, not yet an enabled idle/suspend policy. `face` is limited to180 seconds and the scripts to90 seconds. Existing production power/compositor code is unchanged. The service-restart handoff closes UI apps; production needs integrated compositor/proxy ownership instead.

Build from this directory using the repository's nix-shell cargo cross-build and required patchelf steps. Binaries: hoki-lp-watchface, hoki-suspend-check. Display binary runs as ceres; suspend helper/root orchestration runs as root. Hardware libraries are loaded dynamically.

The custom Hoki layer now includes these two binaries under `/usr/lib` so they
survive image replacement. It does not enable the pilot at boot or install an
automatic suspend policy. The historical task-specific orchestration below
still requires its explicit recovery owner; packaging alone does not authorize
running the display handoff without that recovery arrangement.

`hoki-lp-watchface preflight 1`: query limits only.
`hoki-lp-watchface face 70`: upload/commit font and backing, give BG display ownership, wait for timeout/SIGTERM, release.
`hoki-lp-watchface release 1`: bounded recovery API (must run after display client exits).
`hoki-suspend-check awake 3`: verify alarmtimer expiration without suspend.
`hoki-suspend-check mem 25`: arm kernel-managed wake alarm, use wakeup_count protocol, attempt suspend, report elapsed minus awake clocks and alarm expiration. Any early wake ends test; no repeated suspend loop.

Deployed scripts in /tmp are task-specific and intentionally not boot-enabled. Outer unit MUST have ExecStopPost=/tmp/hoki-lp-recover-0182.sh, RuntimeMaxSec=90 and TimeoutStopSec=5. Never run trial.sh directly without that recovery owner.

Limits and transition/health-logging requirements: ../_Tasks/0182_Low_Power_Watchface/design.md. Test results: corresponding summary.md. Active graphics service must not be restarted for probing. Resource-allocation failure-boundary tests are deliberately excluded after earlier vendor instability.
