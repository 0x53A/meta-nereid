## Hoki Venus SSH development backend (2026-09-28)

Historical record from before the adapter extraction. For the current standalone
`dev-ssh` feature, see [the adapter README](../README.md).

`PICT_NEREID_VENUS=1` with the Nereid SSH backend selects native on-watch SHM
capture, NV12 conversion and Qualcomm Venus H.264 encoding. PC relays encoded
access units into existing str0m transport; it does not encode frames. See
[worker build/protocol notes](../../../../tools/nereid-venus.md). Two requests
maximum in flight; capacity-one transport handoff; stale chains require IDR.
Watch compositor timestamps determine RTP time. Request-to-send metric includes
read-ahead/network waiting and is not input latency. Native client decoded412
416x416 Doomframes at33.5–34fps without discontinuities after Nereid rendering
optimisation. Driver cleanup FLUSH_DONE verified on normal disconnect. No radio
power settings changed by worker. Current live session resize requires reconnect;
raw-Zstd/preview-only modes rejected. Native Wayland/encoder worker owns all
watch buffers; maintained C sources beside SSH helper, not under task folders.
