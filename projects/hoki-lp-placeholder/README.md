# Low-power watchface Wayland companion

`hoki-lp-placeholder FACE_ID` reads the installed ambient face manifest and shows
its name with **Uploading… / Please wait**. It is a static Slint/Wayland surface
owned by the selected LP face, not a replacement for the configured primary
watchface. The compositor manages its process, visibility, frame callbacks and
hardware handoff. Do not invoke Sidekick APIs from this app.

The companion is declared by each manifest's `placeholder` argv array. Use a
different packaged Wayland application there to provide a matching interactive
face later. Missing fields retain the standard companion for old manifests.

Build from this directory using the repository's nix-shell Cargo and ELF-patching
workflow. It installs to `/usr/lib/hoki-lp-placeholder` through runtime-projects.txt.

For headless renderer captures and circular-bound checks:

```sh
nix-shell --run 'HOKI_PLACEHOLDER_CAPTURES=/tmp/lp-preview cargo test --locked'
```
