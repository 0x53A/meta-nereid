# Watch audio

Audio follows the Connect app's round-screen layout, with a warm amber palette.
Output/Input selects a circular device list. Tap a row for large volume (5%
steps, 0–150%), mute and selection controls. The selected device is named, and
unavailable devices disable their controls. Refresh rereads PulseAudio; command
errors appear as dismissible messages. Device names wrap on the control screen
and elide in lists. Curved bands are copied from Connect's source SVG assets.

## Interactive AirPlay

Tap **AirPlay** in Outputs to run a finite local-network search. **Search**
repeats it; **Stop**, Back, or closing the app cancels it. The browser has an
8-second deadline and 256 KiB output limit. Nothing scans at app startup, while
using the normal mixer, or after a search finishes. This app never loads
`module-raop-discover` and does not turn on Wi-Fi itself.

Discovery only populates the picker. Selecting a compatible receiver explicitly
loads `module-raop-sink` and selects it as the default output. Changing receivers
first removes the previous sink created by this feature, so there is at most one.
**Remove** on its controls unloads that sink. External audio outputs are left
alone. Closing Audio leaves the chosen sink available for playback; reopening
Audio recognizes it by its PulseAudio property. There is no saved receiver,
startup auto-connect, or automatic reconnect after connection failure.

The backend is legacy RAOP supported by PulseAudio (16-bit stereo at 44.1 kHz,
TCP/UDP, PCM/ALAC, unencrypted/RSA). Password and unsupported encryption/format
advertisements are shown as unavailable. AirPlay 2 pairing is not implemented.
Sink creation confirms configuration, not a successful audible connection;
receiver compatibility must be tested with playback. Existing streams follow
PulseAudio's routing policy when the default changes.

Runtime dependencies: `pulseaudio-server`, `pulseaudio-misc`,
`pulseaudio-module-raop-sink` (and its RAOP library), `avahi-utils`,
`avahi-daemon`. The local Yocto overlay enables OpenSSL for RAOP and packages
these dependencies. Avahi is the shared mDNS service; stopping a search releases
only this app's browser and does not stop the shared daemon. Missing dependencies
produce an explicit error. Installing only the UI binary does not install them.

`/etc/pulse/daemon.conf.d/20-hoki-session.conf` disables the audio server's
20-second idle exit. Otherwise manually selected sinks vanish between apps.
Device suspension remains enabled, so an idle output need not keep streaming.
The server reads this configuration on startup; never restart it over an active
recording merely to apply the change. Both the image overlay and standalone OPK
include it.

## Execution and validation

Mixer queries and changes run in submission order on a background worker.
Independent sink/source request generations prevent stale results from replacing
newer UI state. Changes are followed by server readback. A separate worker runs
discovery, so a search cannot block volume commands. Network names/TXT records
are parsed as data, with supported protocol/codec/format values selected locally.
Only the chosen receiver is passed to `module-raop-sink`.

Each `pactl` process has a 10-second deadline and a combined 4 MiB stdout/stderr
limit. Both pipes are drained without blocking; failures terminate and reap the
direct child. A multi-command job can take more than one deadline. Commands are
not automatically retried: timeout does not prove a server-side change was
unapplied. A failed AirPlay load triggers cleanup by its owned-sink marker.

Run native tests in this project's `nix-shell` with `cargo test --locked`.
Tests cover actual layout hit targets, disabled controls, ordered work, stale
replies, parsing, cancellation, command bounds, selected-only sink creation and
failed-load cleanup without a real audio server or receiver.

Software-renderer previews need no desktop display, audio server, or network:

```sh
nix-shell --run 'cargo run -- --preview detail /tmp/audio-detail.ppm'
```

States: `outputs`, `inputs`, `detail`, `muted`, `long`, `missing`, `busy`,
`airplay-selected`, `airplay`, `searching`, `search-empty`, `search-error`,
`empty`, `error`, `loading`. Captures apply the physical circular panel mask.
They verify layout, not on-wrist readability or physical touch performance.

Build/deployment follows `CLAUDE.md`.
