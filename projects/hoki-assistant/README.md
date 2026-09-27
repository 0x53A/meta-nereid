# Watch assistant

`hoki-argyroneta` speaks through the watch while Argyroneta on Android performs
speech recognition, inference and phone tools. It uses authenticated Classic
Bluetooth RFCOMM data, independent of headset/call profiles. The selected phone
and watch must already be bonded. VAP is deferred for future LE Audio hardware;
see [backend design](../../../knowledge/assistant-backends.md).

## Build

From this directory, using its own environment:

```sh
nix-shell --run 'cargo test'
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-argyroneta'
```

The runtime inventory includes the binary, launcher, desktop entry and BlueZ
D-Bus policy. `../../build-runtime.sh` rebuilds the complete runtime bundle.
No assistant is selected automatically by installing the package.

For a desktop rendering without Bluetooth/microphone access:

```sh
nix-shell --run 'cargo run -- --preview listening --capture /tmp/assistant-listening.png'
```

Preview modes are `listening`, `error`, and `reply`. They require a desktop
Wayland/X11 session; the action button is inert in preview.

## Setup when devices are available

1. Install the matching compositor/watch package and Android APK. The policy
   `deploy/org.hoki.assistant.conf` belongs in `/etc/dbus-1/system.d/`.
2. Pair the phone and watch using their normal Bluetooth settings.
3. On Android, open Argyroneta assistant setup → **WATCH ASSISTANT**, allow
   Bluetooth/speech permissions, and enable the paired watch. Its connected
   device foreground service exposes only the private assistant UUID and checks
   every connection against this selected bonded device. Disable it from the
   setup screen or persistent notification. The service does not auto-start at
   boot; reopen this screen and restart the connection after a phone reboot.
4. As watch user `ceres`, create `~/.config/hoki/argyroneta.json` with
   `{"phone":"AA:BB:CC:DD:EE:FF"}`, using the phone's actual Bluetooth address.
5. As `ceres`, run `/usr/lib/hoki-argyroneta --register`. This persists
   `agent=/usr/lib/hoki-argyroneta --role` in the compositor shell configuration.
   The existing private compositor socket accepts `get-agent` and
   `set-agent <shell-quoted argv>`; empty `set-agent` disables the slot.
6. Hold the crown for 650 ms. Tap **Send** to finish speaking, or wait for the
   phone recognizer to end the utterance. The watch displays status/transcript
   and plays synthesized speech. Crown tap / top button / **Close** dismisses;
   bottom button activates the displayed Speak/Send/Cancel action.

Crown activation preserves the previous app. A short crown tap retains normal
navigation on release when an assistant is configured. A wake tap is consumed;
a wake-and-hold can activate. Hiding/dismissing the role closes the session and
stops capture/playback. It does not undo a phone tool action already executed.
Requests are never automatically replayed after disconnect.

## Protocol v1

UUID `494ff010-8ff4-4da7-9c56-ead025905e53`, secure RFCOMM. One request per
connection; reconnect only after a new user request. Each frame has `u8 kind`,
`u32 big-endian payload length`, payload; maximum 65,536 bytes before allocation.
JSON and text use UTF-8. PCM is raw signed little-endian 16-bit.

| Kind | Direction | Payload |
| --- | --- | --- |
| 1 HELLO | Watch → phone | JSON version=1, rate=16000, channels=1, encoding=s16le |
| 2 READY | Phone → watch | Empty; recognition accepts microphone audio |
| 3 PCM | Watch → phone | Even, nonempty, at most 3,200 bytes |
| 4 END | Watch → phone | Empty; closes recognizer input, once only |
| 5 CANCEL | Watch → phone | Empty; disconnect also cancels |
| 6 STATE | Phone → watch | JSON phase and text |
| 7 AUDIO_FORMAT | Phone → watch | JSON rate (8–48 kHz), channels (1–2), encoding=s16le |
| 8 AUDIO | Phone → watch | Response PCM after AUDIO_FORMAT |
| 9 DONE | Phone → watch | Final response text; end of response audio |
| 10 ERROR | Phone → watch | Displayable failure |

Audio starts only after READY. Watch capture explicitly uses PulseAudio source
`hoki_microphone`; response playback uses `pacat` on `hoki_speaker` with a
dedicated stream identity and unity stream gain (speaker master volume is
preserved). Input is capped at one minute,
response audio at 8 MiB, and the session at two minutes. Queues are bounded;
slow/stalled peers time out. The watch holds the optional power daemon's CPU and
display inhibitor for the session. Shutdown closes Bluetooth and terminates
capture/playback children before the UI process returns.

Android supplies a pipe to the existing on-device recognizer, and runs a local
AgentModel in its foreground service without opening a phone activity. This
path does not create a phone microphone recorder. The existing phone assistant
continues to use its normal capture source. Existing agent trace behavior still
applies to transcripts/inference; the bridge does not retain incoming PCM.
TTS uses Android's configured engine; an engine without supported synthesis PCM
callbacks yields text only. Its network behavior depends on the chosen engine.

## Device validation still required

No device was accessed during development. Verify recognition honors supplied
PCM (and does not substitute the phone mic), background/locked-phone inference,
Bluetooth discovery and routing, microphone/speaker quality, delayed Send,
cancel/disconnect during each phase, phone recorder contention, TTS callback
format, and returning to the prior watch app. Actual Bluetooth throughput and
phone power management are not established by local compilation or wire tests.
