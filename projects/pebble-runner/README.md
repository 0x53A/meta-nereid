# Pebble runtime

The maintained Pebble application/watchface runtime lives here. It was moved
from `_Tasks/0022_Pebble_Compat/pebble-runner`; the historical investigation and
headless test results remain in [task 0022](../../../_Tasks/0022_Pebble_Compat/readme.md).
The Pebble OS reference source is the root `pebble-os/` submodule.

Build from this directory using its Nix environment:

```sh
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh target/armv7-unknown-linux-gnueabihf/release/pebble-runner'
```

For host tests, run `nix-shell --run 'cargo test --locked'`. Cargo fetches the
pinned armagnac fork revision, including the required ARM instruction extensions.
Required `.pfo` font assets are included
under `fonts/`; downloaded `.pbw` test applications remain optional local inputs.

Image packaging uses [runtime-projects.txt](../../runtime-projects.txt).
Watch deployment and service ownership are documented in [CLAUDE.md](../../../CLAUDE.md).
