# Nereid projects

Application, compositor and service sources live here, with common Rust source
modules in `shared/`. Image builds compile these sources with BitBake; see
[the build inventory](../APPS.md) and [image tooling](../tools/README.md).

## Rust workspace

The 24 runtime packages selected by `runtime-projects.txt` share this directory's
`Cargo.toml`, `Cargo.lock` and `target/`. Add dependency versions to
`[workspace.dependencies]` and inherit them in members with `workspace = true`.
Member-specific features remain explicit. Slint and slint-build are pinned
together at 1.15.1. No direct dependency currently needs a per-app version
exception; transitive libraries can still require different major versions.

From an app's own Nix shell, `cargo build --locked --release --target
armv7-unknown-linux-gnueabihf` still selects that app. Its output is now in
`../target/armv7-unknown-linux-gnueabihf/release/`. From this directory use
`cargo build --locked -p hoki-settings` to select a package. BitBake provides
the complete sysroot and builds all enabled members together; a single app's
Nix shell need not contain the native libraries required by every workspace app.

The default member set omits the disabled WASM demo. `--workspace` explicitly
selects all members, so use `--exclude hoki-wasm-host` unless its guest has been
built. BitBake's `HOKI_WASM_DEMO` switch handles this automatically. BLE SSH,
health recording and the WASM guest remain explicitly excluded standalone
projects with separate recipes/toolchains and lockfiles.

For ARM tests, first build `asteroid-image qemu-native` on frost-8000. Inside a build
container with host `dbus-daemon` installed, run
`python3 /asteroid/meta-nereid/tools/test-runtime-workspace.py /asteroid/asteroid/build`.
This reuses the recipe's compiler and the image's runtime libraries, plugins and
XKB data, then runs test binaries under QEMU;
it does not connect to a watch.

Release profiles live here too: common size optimization, LTO and stripping,
with speed optimization retained for Music, Pebble and their codec/emulator
code. Avoid per-app profiles that silently split dependency compilation.

The layer's `runtime-projects.txt` selects UI/runtime binaries. Bluetooth SSH and
health recording have separate source recipes. The SSC helper uses the pinned
Android NDK native recipe, and the embedded WebAssembly guest is source-built.
GPS-recorder and NFC integration also use direct source recipes. Acoustic SSH
remains in its own pinned GitHub repository.

Cargo pins armagnac, bluer and dbus-rs to revisions recorded in manifests and
lockfiles. BitBake fetches those revisions, including BlueR's pinned Bluetooth
database submodule. After lockfile or project inventory changes, regenerate
recipe inputs with `python3 ../tools/update-runtime-recipes.py` from this directory.

Nix shells and bundle builders remain available for standalone development and
direct deployment. For those builds, use each project's own shell and the layer's
`patch-watch-elf.sh` helper. Generated output stays ignored; private inputs and
captures remain in the workspace's `data/` directory.
