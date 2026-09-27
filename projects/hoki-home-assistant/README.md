# Home Assistant watch remote

Three direct actions fit the round display, with a separate Speak control. The labels and service calls live in
the watch user's private config, so personal entities and the access token stay
out of the source tree and runtime image. A tap sends one REST service call. The
status says **Request accepted** when Home Assistant returns a successful HTTP
response; it does not assert the resulting entity state. Network work runs off
the UI thread, with one request at a time and a 12-second timeout.

Create `/home/ceres/.config/hoki/home-assistant.json` on the watch, owned by
`ceres` with mode `0600`:

```json
{
  "url": "https://your-home-assistant-address:8123",
  "token": "YOUR_LONG_LIVED_ACCESS_TOKEN",
  "voice": {},
  "actions": [
    {"label": "Living room", "domain": "light", "service": "toggle", "data": {"entity_id": "light.living_room"}},
    {"label": "Good night", "domain": "script", "service": "turn_on", "data": {"entity_id": "script.good_night"}},
    {"label": "Desk scene", "domain": "scene", "service": "turn_on", "data": {"entity_id": "scene.desk"}}
  ]
}
```

Replace all example entities with real ones. Zero to three actions are supported
when `"voice": {}` is present; otherwise configure one to three actions. Remove
`voice` to hide Speak. To select a specific Assist pipeline, set
`"voice": {"pipeline": "PIPELINE_ID"}`; an empty object uses HA's preferred
pipeline.
The `data` object is sent unchanged to `POST /api/services/<domain>/<service>`.
Use HTTPS for the watch to HA connection because the bearer token otherwise
travels in cleartext. Keep config mode `0600`; the app refuses wider permissions.
No token or action request is logged.

## Speak

Tap **Speak** to open a dedicated recording screen. The app authenticates to
`/api/websocket`, starts HA's Assist pipeline at STT and ends at intent, then
captures 16 kHz mono signed 16-bit PCM from the watch's `hoki_microphone` source.
It streams each chunk with HA's handler byte after `stt-start`. Tap **Send** to
finish manually; HA's end-of-speech event can finish it automatically. **Cancel**
closes the connection and microphone capture. The watch shows the recognized
text and HA's response text. This version does not play TTS audio.

Capture is capped at one minute and the voice session at two minutes. The app
keeps CPU/display awake during the request when `hoki-powerd` is available,
and drops that hold on completion or cancellation. Audio is streamed, not saved.
Voice requires an Assist pipeline with STT and an intent agent configured in HA.

Build from this directory:

```sh
nix-shell --run 'cargo test'
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-home-assistant'
```

`nix-shell --run 'cargo run -- --preview --capture /tmp/home-preview.png'`
renders inert example actions for design review. Use `--preview-voice` for the
recording screen or `--preview-reply` for a long answer. The runtime bundle builder
packages the app and launcher; no watch deployment is implied by a local build.

The WebSocket client and microphone path are built locally. Actual HA pipeline
behavior and wrist microphone performance still require on-device testing after
the HA pipeline is configured.
