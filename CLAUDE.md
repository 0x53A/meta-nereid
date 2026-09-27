# Nereid development

Use `Lukas Rieger <code@lukasrieger.com>` for project attribution and preserve
third-party authorship. Application sources and shared modules are in projects/.
Build from each project's own directory using its shell.nix. Runtime builders
and ELF patching helpers live at this layer's root. Keep generated binaries,
archives, build caches and private data ignored.

UI/runtime Rust packages share projects/Cargo.toml, projects/Cargo.lock and
projects/target/. Dependency versions and release profiles belong at the workspace
root; members inherit dependencies. BLE, recorder and WASM guest remain excluded
standalone projects. BitBake builds enabled runtime members together.

When used inside asteroid-watch, follow its root CLAUDE.md for watch access,
image building, deployment and preserving captures. A source edit or local test
does not authorize deploying or restarting watch services. Application-specific
CLAUDE.md files still apply. Image builds compile runtime components with BitBake. Regenerate recipe inputs
with `python3 tools/update-runtime-recipes.py` after Cargo.lock/project inventory
changes. Nix bundle builders remain for standalone development; rebuild those
bundles before using them for direct deployment.

Current manual health recording and Settings controls are documented in
[the recorder notes](projects/hoki-health-recorder/CLAUDE.md) and
[the Settings notes](projects/hoki-settings/CLAUDE.md). These distinguish local
validation from deployment and the manual HAL service from combined SSC trials.
