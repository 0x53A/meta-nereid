## Native watch daemon (2026-09-28)

Historical record from before the adapter extraction. For current build commands,
features and paths, see [the adapter README](../README.md).

The host package exposes a shared Rust library used by the desktop application
and `pict-watchd`. Default desktop features retain the existing app; the watch
build uses `--no-default-features --features watch` and excludes the UI, D-Bus,
PipeWire and FFmpeg. Local Wayland/uinput plus the existing native Venus worker
replace SSH/Python. See [deployment and limits](deploy/watch/README.md).
Pairing and authorization remain shared; the daemon never auto-approves unknown
clients. Watch UI/QR/comparison pairing are separate follow-ups. The initial
headless service is manually started, not boot-enabled.
