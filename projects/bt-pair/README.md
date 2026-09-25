# bt-pair

Bluetooth pairing manager for AsteroidOS, using [Slint](https://slint.dev/) as the UI framework running as a Wayland client.

## What it does

Manage Bluetooth devices from your watch:
- View paired devices and their connection status
- Tap to connect/disconnect
- Unpair devices you no longer need
- Scan for new devices and pair them

All Bluetooth operations run in the background so the UI stays responsive.

## Building locally (desktop preview)

```bash
cd bt-pair
cargo run
```

This will open a window on your desktop — Slint auto-selects the best backend (Wayland if available, otherwise X11/software).

## Cross-compiling for AsteroidOS

Use the provided deploy script:

```bash
nix-shell --run ./deploy.sh
```

This will:
1. Cross-compile for armv7-unknown-linux-gnueabihf
2. Patch the ELF interpreter (Nix linker path → standard `/lib/ld-linux-armhf.so.3`)
3. Deploy via ADB to your watch

## Project structure

```
bt-pair/
├── Cargo.toml          # Dependencies & release profile
├── build.rs            # Slint build script
├── deploy.sh           # Build, patch, and deploy to watch via ADB
├── build-opk.sh        # Build .opk package
├── shell.nix           # Nix dev shell with ARM cross-toolchain
├── src/
│   └── main.rs         # App logic & bluetoothctl integration
├── ui/
│   └── main.slint      # UI layout (round watch face)
└── deploy/
    ├── bt-pair.sh        # Launcher wrapper (→ /usr/bin/)
    └── bt-pair.desktop   # App launcher entry (→ /usr/share/applications/)
```
