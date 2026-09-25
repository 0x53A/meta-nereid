# WASM guest demo

Exports `init(width, height)`, `render()`, `on_touch(x, y, pressed)` and
`buffer_len()`. The host initializes the framebuffer, sends touch events, then
copies `buffer_len()` bytes at the pointer returned by `render()` from WASM
linear memory. Pixels are RGBA8. Reacquire the memory view after guest calls;
initialization can replace the framebuffer allocation.

Guest state is protected by a mutex. The current host serializes calls through
one wasmi Store; the guest has no imported callbacks. Rendering uses squared
distance for coarse ring bounds and an inverse-square-root approximation for
the final distance comparison.

Run `cargo test` for the native pixel regression. Build the guest with
`cargo build --locked --release --target wasm32-unknown-unknown`.
The runtime builder rebuilds this module and embeds it in `hoki-wasm-host`;
rebuilding only the guest does not update an already-built host binary.
