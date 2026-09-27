# Settings development notes

Follow the containing layer/workspace CLAUDE.md. Build in this directory using
shell.nix. Workspace output is ../target/; after a standalone ARM release build,
use ../../patch-watch-elf.sh on that binary. Image builds compile the workspace
with BitBake and use the shared ../Cargo.lock. Deployment is owned by the
authorized session.

Storage follows Battery as a single percentage/used/total summary, opening a
Total/Used/Available/Reserved subpage. src/storage.rs reads statvfs on /var/lib
through the five-second background poll. Used excludes free blocks; available
is f_bavail, excluding filesystem reserves. Hidden managed applications retain
the existing poll gate. Keep crown indices, touch indices, row count and acoustic
slider position aligned when adding rows.

CPU cores and Auto cores are adjacent. Boolean rows use explicit 48px checkbox
activation regions; tapping their labels only selects the row. Screen Off is
available through watchface controls, not the Settings list. Health & sleep groups
manual recording, automatic sensor profile and the power daemon's sleep reason.

Both side buttons and the touch Back footer return detail pages to the main list,
preserving selection. Power is an overlay with four thinner actions: Power off,
Reboot, Bootloader and Back. Its upper button powers off; lower button goes Back.

Health recording controls the fixed system unit hoki-health-recording.service
using nonblocking start/stop jobs. It does not enable recording at boot. Status
polling distinguishes unavailable, starting, active, stopping and failed; a
starting capture can be stopped. ceres gets only scoped start/stop authorization.
The recorder currently restarts sensorfwd during start/cleanup; see the recorder
project CLAUDE.md for operational limits. No suspend policy is implied by On.

Renderer regression coverage uses actual pointer input and production side-button
callbacks, including busy actions, checkbox-only toggles, optional acoustic rows,
and all subpage exits. Optional HOKI_SETTINGS_TEST_CAPTURES writes renderer images.
See _Tasks/20260926_Settings_Back_Navigation in the containing repository for
current build/visual validation and deployment status.
