# ARM link-only stubs

These are generated link inputs, not copies of the watch's actual GLib/GStreamer
implementations. The development shell builds them through `default.nix`; the
resulting `.so` files stay in the Nix store. Old local `.so` files are ignored and
are no longer used by the shell.

`symbols/` preserves the exported function names from the existing stubs.
`manifest.json` records their SONAMEs, export counts and original binary hashes.
`generate.py` builds replacement ARM shared objects with the same exports and
SONAMEs without requiring a device or a pre-existing binary.

These files supply symbols for cross-linking only. Never install or execute
them on the watch; the real implementations must already be installed there.
The symbol inventories preserve the previous stubs' behavior, including symbols
represented as functions; this does not establish a complete runtime ABI model.

To rebuild independently, run `nix-build ./cross-lib --no-out-link` from
`hoki-audiobook/`.
