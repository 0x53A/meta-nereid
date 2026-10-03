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
The main-list PIN Management row launches `hoki-lockscreen --manage-pin`
through the compositor's `launch-argv:` role message. Keep its touch index,
crown count and scroll extent aligned with the row.
The following Lock now row calls `io.Nereid.Auth1.Lock` through the background
action worker. With no PIN, it explains that PIN setup is needed; the service
retains its optional-PIN behavior. Screen off is separate from authentication lock.

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

The activity integration deployed on 2026-09-28 makes ordinary manual recording a `full`
consumer lease, with the automatic profile another consumer. The shared broker
changes intermediate demands without restarting sensorfw. First/last-consumer
setup/cleanup still has the restart limitation. The Settings UI continues to
control the same fixed unit; see the recorder's shared-consumer notes.

Networks opens a saved-Wi-Fi selector plus diagnostics. Saved services are
ConnMan Favorite/Immutable Wi-Fi entries; Connect/Disconnect revalidate the
object path at action time. No passwords, network removal, or provisioning are
exposed. Scan and radio-on are explicit actions; polling only reads state.
List and detail scrolling support touch and crown. Back returns from details or
diagnostics to the selector, then to Settings. Diagnostics include connected
services, addresses, gateway/DNS, interfaces and system resolver settings;
ConnMan's online state is reported, with no independent Internet probing.
