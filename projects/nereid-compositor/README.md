# nereid-compositor

Minimal Rust Wayland compositor for AsteroidOS, targeting the Fossil Gen 6 (hoki).
Renders via libhybris hwcomposer EGL backend, replaces the Qt/lipstick compositor stack.

Hyprland-style: the compositor only composites surfaces and routes input.
Watchface, launcher, notifications, etc. are separate Wayland client apps.

## Build

Requires rustup with `armv7-unknown-linux-gnueabihf` target installed.

```sh
nix-shell
cargo build --release --target armv7-unknown-linux-gnueabihf
```

## Package

```sh
nix-shell --run ./build-opk.sh
```

Produces `nereid-compositor_0.1.0_armv7vehf-neon.opk` (~470K).
Conflicts with and replaces: `asteroid-launcher`, `lipstick`, `qt5-qpa-hwcomposer-plugin`,
`asteroid-launcher-configs`, `qtscenegraph-adaptation`, `mapplauncherd`,
`mapplauncherd-qt`, `mapplauncherd-booster-qtcomponents`.

## Install

```sh
scp nereid-compositor_0.1.0_armv7vehf-neon.opk root@hoki.local:/tmp/
ssh root@hoki.local 'opkg install --force-conflicts /tmp/nereid-compositor_0.1.0_armv7vehf-neon.opk'
```
