# WASM demo host

Embeds the sibling `hoki-wasm-guest` module and displays its 208×208 RGBA8 frames
upscaled to the watch's 416×416 window. Touch coordinates are mapped into the
guest's render coordinates. Calls share one wasmi Store on the UI thread.

Before copying a frame, the host checks its exact expected byte count and its
range within current WASM memory. A render, buffer-length or touch-call error
stops the animation timer, preserves the last image, and displays
“Renderer stopped”. Details go to stderr. The startup-status timeout does not
erase this failure message. Initial module loading still fails fast if the
embedded module or required exports are unavailable.

Run `nix-shell --run 'cargo test --locked'` from this directory. The tests use a
headless Slint platform and do not open a window or contact the watch. Frame-range
tests can also be compiled independently from `src/frame.rs` with `rustc --test`.

Use `meta-nereid/build-runtime.sh` from the repository root to rebuild the
guest, embed it in the ARM host, patch the host loader, and validate the local
runtime archive. Rebuilding the guest alone leaves an existing host unchanged.
