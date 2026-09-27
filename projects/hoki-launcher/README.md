# Hoki launcher

The main list shows nonempty folders first, then everyday apps:

- **AsteroidOS**: desktop IDs beginning with `asteroid-` (except our demos).
- **Demos & Tests**: Rust Demo, Egui Demo, WASM Demo, IMU Test, Counter,
  Crab Rave, and the legacy Hoki watchface app.
- **Tools**: BT Pair, Audio, NFC, and Connect.

Tap a folder or select it with the crown and bottom button. Each folder has an
**All apps** back row; the top button also returns to the main list, preserving
its selection. At the main list the top button retains its existing Settings
shortcut. Settings and system-wide navigation are unchanged.

App icons are rendered in white for contrast on the black display and come from desktop `Icon=` entries (absolute image paths or SVG/PNG
names in the installed Asteroid and hicolor themes, or pixmaps). Missing or
unreadable icons leave a text-only row. Folder/back symbols are bundled vectors.
Selected rows and folder navigation use the launcher's blue accent. Desktop
entries marked `Terminal=true` and HTop are omitted because the watch launcher
does not provide a terminal.

Optional configuration: `$XDG_CONFIG_HOME/hoki-launcher.json`, defaulting to
`~/.config/hoki-launcher.json` for the UI user, ceres. For example:

```json
{
  "icons": false,
  "folders": {
    "hoki-connect-ui": "",
    "hoki-audiobook": "Media",
    "hoki-music": "Media",
    "hoki-podcast": "Media"
  }
}
```

Keys are desktop filenames without `.desktop`; an empty folder keeps an app at
the top level. A desktop entry can also declare `X-Hoki-Folder=Tools`.
Pebble Runner uses this field for downloaded PBW shortcuts in **Pebble apps**.
User configuration wins over desktop metadata, which wins over built-in defaults.
Folder names are literal labels; nesting is not supported. Configuration and
installed apps are loaded when the launcher starts and refreshed after an app
closes. The launcher scans `/usr/share/applications` and the UI user's
`$XDG_DATA_HOME/applications` (default `~/.local/share/applications`); user
desktop IDs override system IDs. Invalid JSON is logged and falls back to
defaults.

For local fixtures, override `HOKI_APPLICATIONS_DIR`, `HOKI_LAUNCHER_CONFIG`, and
`HOKI_ICON_PATH` (a colon-separated list of icon directories).

Build from this directory:

```sh
nix-shell --run 'cargo test'
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/hoki-launcher'
```

The UI regression test uses the actual Slint software renderer; set
`HOKI_LAUNCHER_TEST_CAPTURES` to an output directory to retain its PPM previews.
