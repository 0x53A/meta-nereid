# demo-asteroid-app

A minimal Rust app for AsteroidOS, using [Slint](https://slint.dev/) as the UI framework running as a Wayland client.

## What it does

A simple counter app sized for a 320×320 round watch face. Tap **+1** to increment, **Reset** to zero it out. Demonstrates:

- Slint UI rendering on Wayland
- Touch input handling
- Callback wiring between Rust logic and Slint UI

## Building locally (desktop preview)

```bash
cd demo-rust-app
cargo run
```

This will open a 320×320 window on your desktop — Slint auto-selects the best backend (Wayland if available, otherwise X11/software).

## Cross-compiling for AsteroidOS (Yocto)

1. Add `meta-rust` (or `meta-rust-bin`) to your Yocto build layers.

2. Create a BitBake recipe (or use the one in `deploy/`):
   ```bitbake
   inherit cargo
   SRC_URI = "file://demo-rust-app"
   ```

3. Build:
   ```bash
   bitbake demo-asteroid-app
   ```

4. The resulting binary and `.desktop` file go to the watch.

## Quick deploy via SCP (development)

If you have SSH access to the watch (USB or Wi-Fi):

```bash
# Cross-compile (adjust target as needed for your watch)
cargo build --release --target armv7-unknown-linux-gnueabihf

# Push to watch
scp target/armv7-unknown-linux-gnueabihf/release/demo-asteroid-app root@192.168.2.15:/usr/bin/
scp deploy/demo-asteroid-app.desktop root@192.168.2.15:/usr/share/applications/
```

Restart the launcher or reboot to see it in the app list.

## Project structure

```
demo-rust-app/
├── Cargo.toml          # Dependencies & release profile
├── build.rs            # Slint build script
├── src/
│   └── main.rs         # App logic & callback wiring
├── ui/
│   └── main.slint      # UI layout (320×320 watch face)
├── deploy/
│   └── demo-asteroid-app.desktop  # App launcher entry
└── README.md
```
