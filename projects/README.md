# Nereid projects

Application, compositor and service sources live here, with common Rust source
modules in `shared/`. Build each project from its own directory using its Nix
shell. Layer build and packaging helpers are two directories above; for example,
`bash ../../patch-watch-elf.sh <binary>`.

The layer's `runtime-projects.txt` selects UI/runtime binaries. BLE SSH and
health-recorder have separate bundle builders. GPS-recorder and NFC integration
sources are consumed directly by BitBake recipes. Acoustic SSH remains in its
own GitHub repository.

Cargo pins armagnac, bluer and dbus-rs to GitHub revisions recorded in manifests
and lockfiles. No nested library checkout is required. Generated output stays
ignored; private inputs and captures remain in the root repository's `data/`.
See `../../APPS.md` for current build requirements and migration limitations.
