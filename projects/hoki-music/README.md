# Music for hoki

A new Rust music player with a red-violet and gold Slint interface following the
Connect layout. Symphonia decodes music; the service sends PCM through libpulse.
Audio decoding, buffering and playback logic remain Rust, with libpulse as the
audio boundary. No GStreamer, mpv, FFmpeg, external player process or native codec
is used. TLS defaults to the system implementation (OpenSSL on Linux); a pure
Rust alternative is available as a Cargo feature. The UI uses native platform
libraries.

## Playback and library

- MP3, FLAC, AAC-LC/ALAC in M4A, Ogg Vorbis, PCM WAV and AIFF; mono/stereo.
- On-device files in `~/Music` by default, including nested folders and tags.
- Navidrome original-file streaming over HTTP(S), with byte-range seeking.
- One mixed-source queue, previous/next, pause, seek in 15-second steps, stop,
  and a player volume control. Select the output in the Audio app; active
  playback follows default-output changes through PulseAudio events.
- A separate user service keeps playback alive when the UI closes. Reopening
  reconnects to it. After a service restart, the saved queue is restored paused;
  music never starts automatically.
- Initial local scan for an empty library; subsequent refreshes are explicit.
  No idle library/network polling. Playback thread waits for commands while
  stopped/paused; UI requests state every 750 ms while running.

Tap the center track information for seeking and Stop. The crown/arrow keys
adjust player volume on the playback page. Sources chooses which library to
refresh; Library lists both sources and tapping a song plays from it onward.

Opus, HE-AAC, DRM and multichannel audio are not supported. There are no offline
Navidrome downloads, album-art display, playlist editing, or strict gapless
transitions in this first version. Libraries are bounded at 10,000 local and
10,000 remote tracks. HTTP redirects and servers without byte-range responses
are rejected; configure the final Navidrome URL and serve original files.

## Configuration

Optional `~/.config/hoki-music/config.json` (create with mode 0600):

```json
{
  "music_dirs": ["/home/ceres/Music"],
  "navidrome": {
    "url": "https://music.example.org",
    "username": "your-name",
    "password": "your-password"
  }
}
```

Omit `navidrome` or set it to null to use local music alone. Config is reloaded
on library refresh and when opening a track. Server credentials never go to the
UI or persisted queue. Authentication uses a random salt and Subsonic token.
Both TLS backends validate certificates. System TLS uses the platform trust store
(`ca-certificates` on the watch); the RustCrypto backend uses bundled WebPKI roots.
The watch currently has OpenSSL 3.5.6. Password entry on the watch and an app-level
custom-CA setting are not implemented. HTTP is available for trusted local
networks but does not encrypt traffic.

State/index: `~/.local/share/hoki-music/state.json`, atomically replaced with mode
0600 after commands and queue transitions. Position is saved on pause/commands,
not written to flash every playback tick. A crash may lose recent progress.
IPC: `$XDG_RUNTIME_DIR/hoki-music/control.sock`, in a private 0700 directory.

## TLS features

Exactly one TLS feature must be enabled:

| Feature | Behavior |
| --- | --- |
| `tls-system` (default) | Reqwest native TLS; dynamically links system OpenSSL on Linux. Uses native platform TLS on macOS/Windows. |
| `tls-rustcrypto` | Rustls with pinned `rustls-rustcrypto` 0.0.2-alpha and bundled roots. The alpha provider remains opt-in. |

Normal desktop builds and the runtime/image script use the default. For the
pure Rust alternative, disable defaults explicitly:

```sh
nix-shell native.nix --run 'cargo test --no-default-features --features tls-rustcrypto'
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf --no-default-features --features tls-rustcrypto'
```

Enabling both backends, or neither, produces a compile-time error. `--all-features`
is therefore intentionally invalid. Test both configurations separately. Neither
backend vendors an OpenSSL copy. System builds do not compile the alpha provider.

```sh
nix-shell native.nix --run 'cargo test'
nix-shell native.nix --run 'cargo test public_https -- --ignored'
nix-shell native.nix --run 'cargo test --no-default-features --features tls-rustcrypto public_https -- --ignored'
```

The explicit HTTPS test makes external requests: one valid certificate must work,
and one expired certificate must fail. `--check-tls https://example.org/` provides
the same verified-handshake diagnostic on the watch without server credentials.

## BitBake/image integration

Music is registered in `meta-nereid/runtime-projects.txt`, the app package
inventory and the image app packagegroup. `build-runtime.sh` builds it through its
own Nix shell with the default system TLS feature, and copies its launcher and
on-demand user service. The archive validator checks the service bytes too.

The Nix cross shell supplies **target** OpenSSL headers/libraries through its
ARM pkg-config wrapper. The BitBake payload recipe declares OpenSSL/PulseAudio
build dependencies for shared-library packaging and includes `ca-certificates`
at runtime. Shared-library scanning resolves libssl/libcrypto/libpulse package
names. The existing runtime archive must be rebuilt before the next image build;
adding a project does not update an already-built archive.

If migrating to a recipe that compiles Cargo directly, use the Yocto target
sysroot, `DEPENDS += "openssl pulseaudio"`, and the default Cargo features. Do not
pass host OpenSSL paths or enable `native-tls-vendored`. Desktop `native.nix`
supplies OpenSSL, pkg-config and CA certificates independently of the ARM shell.

## Building

Run commands **inside this directory**. Cross-link against the watch's PulseAudio
client ABI (one-time sysroot setup, use the documented Tailscale fallback if needed):

```sh
mkdir -p cross-lib
scp root@hoki.local:/usr/lib/libpulse.so.0 cross-lib/libpulse.so.0
ln -sf libpulse.so.0 cross-lib/libpulse.so
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-music'
nix-shell -p patchelf --run './build-package.sh'
```

The cross sysroot binary is ignored by Git. Native checks/previews:

```sh
nix-shell native.nix --run 'cargo test'
nix-shell native.nix --run 'cargo run -- --preview player /tmp/music.ppm'
```

Preview states: player, paused, long, library, empty, sources, seek, error.
`--decode FILE` benchmarks decoding without audio output. `--request JSON` sends
one IPC command; for example `'{"command":"state"}'` or `'{"command":"stop"}'`.
`HOKI_MUSIC_DIRECT=1` lets the desktop UI spawn the daemon without systemd.

Install binary to `/usr/lib/hoki-music`, launcher to `/usr/bin/hoki-music`, desktop
entry to `/usr/share/applications/`, and service to `/usr/lib/systemd/user/`.
Reload ceres's user service manager. The service is started on demand, not enabled
at boot. Before replacing an installed binary, close its UI and stop its own
service; do not restart PulseAudio or other applications.
