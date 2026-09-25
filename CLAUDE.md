# Nereid development

Use `Lukas Rieger <code@lukasrieger.com>` for project attribution and preserve
third-party authorship. Application sources and shared modules are in projects/.
Build from each project's own directory using its shell.nix. Runtime builders
and ELF patching helpers live at this layer's root. Keep generated binaries,
archives, build caches and private data ignored.

When used inside asteroid-watch, follow its root CLAUDE.md for watch access,
image building, deployment and preserving captures. A source edit or local test
does not authorize deploying or restarting watch services. Application-specific
CLAUDE.md files still apply. Existing Nix runtime bundles must be rebuilt after
source or path changes; source-building BitBake conversion is separate work.
