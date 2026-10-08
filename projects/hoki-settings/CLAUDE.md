# Settings development notes

Follow the containing layer/workspace CLAUDE.md and the required Settings section
in `knowledge/ui-design-rules.md` in the containing repository. Build here using
shell.nix. Workspace output is ../target/; patch standalone ARM builds using
../../patch-watch-elf.sh. Image builds use the shared ../Cargo.lock.

## Inventory and interaction contract

The main list is Battery, Storage, CPU cores, Auto cores, Wi-Fi, Bluetooth,
Network, Airplane, USB, optional Acoustic SSH/volume, Health & sleep, Auto sleep,
Face mode, Ambient face, Idle timeout, Brightness, PIN management, Lock now,
Power, Licenses. **Licenses must ALWAYS be last. Never insert entries below it.**
Power is grouped with Lock now as an action, not with connectivity settings.

Storage retains its label, usage summary and chevron, opening Total/Used/
Available/Reserved. src/storage.rs reads statvfs on /var/lib; available excludes
filesystem reserves. State is polled in the background; hidden managed apps
retain the existing poll gate.

Boolean rows use explicit 48px checkbox targets; labels only select. Multiple
choices show their current value and a chevron, then open the shared choice
modal. Opening/highlighting a choice changes nothing. Touch selection or the
bottom button applies an absolute value; top hardware button and touch Back
cancel. The crown stays in the modal. Fixed detail pages cannot scroll the
hidden main list. Touch and hardware entry use the single activate_row map in
src/main.rs. Keep Slint positions, row counts and acoustic slider offsets aligned.

Bluetooth choices are Off, BLE only, BLE + Classic. hoki-radiod reads the kernel's
observed management settings and owns changes; ConnMan owns radio power.
USB choices are Network, Network + ADB, Charging only. developer_mode is USB
networking; adb_mode includes that networking as well as ADB. usb-moded owns the
gadget, and disconnected/unknown state must not be presented as a chosen mode.

Health & sleep offers the automatic sensor profile and sleep status. There is no
manual recording switch: it was another `full` consumer lease, redundant with the
Full profile and confusing when that lease kept recording after profile Off.
The diagnostic recording service and existing captures are retained; removing
the UI does not stop a running capture or remove a lease.

PIN management launches hoki-lockscreen --manage-pin through the compositor's
launch-argv message. Lock now calls io.Nereid.Auth1.Lock via the background
worker; without a PIN it explains setup. Screen off is a watchface control,
separate from authentication lock. Power uses its existing action overlay:
Power off, Reboot, Bootloader, Back; upper button powers off, lower goes Back.

Network lists only saved ConnMan Favorite/Immutable Wi-Fi services. No password
entry, network deletion or provisioning. Connect/Disconnect revalidate service
paths. Scan and radio-on are explicit; polling only reads state. Turn on sends
an absolute enable, never a toggle derived from a different snapshot. Nested
Back returns detail/diagnostics to the selector before returning to Settings.
Diagnostics are local state, not an independent Internet reachability test.

Renderer tests exercise production pointer input and button callbacks, busy
states, modal cancellation, optional acoustic layouts and nested Network pages.
HOKI_SETTINGS_TEST_CAPTURES writes actual renderer images for visual review.
Do not treat callback-only tests as proof that labels render correctly.
