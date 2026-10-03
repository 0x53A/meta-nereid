# Hoki Venus development backend

Opt-in hardware H.264 encoding on the Hoki watch. Nereid captures its virtual
output into SHM; a native C worker converts BGRA to aligned NV12 and drives the
Qualcomm downstream V4L2/ION encoder. The PC Pict host relays encoded access units
through its existing authenticated WebRTC session. This is **not** the final
standalone watch Pict endpoint: signalling, pairing and WebRTC still run on PC.

Build using the relocated Hoki image SDK and existing toolchain Docker image:

```sh
PICT_WATCH_BUILD=/path/to/hoki/build \
PICT_WATCH_ELF_PATCH=/path/to/meta-nereid/patch-watch-elf.sh \
  tools/build-nereid-venus.sh
```

Install generated `pict-venus/nereid-venus` as executable
`/userdata/pict-demo/nereid-venus`, and `tools/nereid-bridge.py` as
`/userdata/pict-demo/watch-bridge.py`. The worker currently runs as root through
host-key-verified SSH for `/dev/video33` and `/dev/ion` access. Wayland apps still
run as the unprivileged `ceres` user. No watch-side network listener is opened.

Set `PICT_NEREID_SSH=root@WATCH_IP`, `PICT_NEREID_HOST_KEY=hoki.local`,
`PICT_NEREID_VENUS=1`, and `PICT_BITRATE_KBPS=2000` on the PC host. Use the
existing Pict pairing/acceptance flow and grant input separately. The reserved
output is configured at35Hz. Raw-Zstd and preview-only sessions are rejected
in Venus mode. Resizing requires reconnecting the current Venus session.

The codec uses Baseline H.264, CBR, no B frames, repeated headers with IDRs,
a35-frame GOP, explicit keyframe requests and bitrate changes. The driver needs
PREPARE_BUF before QBUF, ION DMA-BUF fds in USERPTR plane reserved[0], and the
Qualcomm FLUSH/FLUSH_DONE sequence before STREAMOFF and release. Native ARM32
V4L2 timeval layout is required even with a time64 SDK.

Two frame requests maximum may be in flight, overlapping network RTT with watch
capture/encode. No request is issued until a bounded transport slot is available.
After a stall, discard the stale dependent chain and require a fresh IDR.
Presentation timestamps come from the compositor, not SSH arrival intervals.
Session shutdown closes stdin, drains pending compressed output and waits for
hardware flush before terminating SSH. No persistent radio/power settings are
changed by this worker.

SSH framing is little-endian. Host request: two u32 values, force-IDR(0/1) and
bitrate bits/sec. Reply: ten u32 values: H.264 byte count, keyframe(0/1), width,
height, capture-wait microseconds, conversion microseconds, encode microseconds,
sequence, compositor presentation microseconds low32, high32; then Annex-B bytes.
Packets are bounded to4MiB. Capture supports even dimensions96..1920 by64..1088
subject to driver limits; arbitrary resolutions are not validated performance
claims. One native SHM buffer and one input buffer are reused only after capture
completion / encoder return respectively.

`capture_to_send_us` is conservatively measured from host request to RTP submission,
including a read-ahead wait; it is not true input-to-display latency. Encode and
conversion fields are measured on the watch. Capture interval is received-frame
interval. PC preview is unavailable for this direct encoded path.
