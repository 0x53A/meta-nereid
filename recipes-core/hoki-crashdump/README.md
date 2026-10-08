# Hoki crash capture

Installed by the Hoki BitBake image and enabled at boot. Dumps survive ordinary
reboots; flashing userdata erases them. Memory dumps are private data.

This kernel sets CONFIG_STATIC_USERMODEHELPER_PATH="", which silently disables
piped core handlers. Instead, Linux writes directly into a **64 MiB disk-backed
ext4 filesystem** at /var/lib/hoki-core-buffer. Its backing image is
/var/lib/hoki-crashdumps/core-buffer.ext4; it uses no tmpfs RAM allocation.
The sysctl path uses an incoming subdirectory which exists only inside the
mounted filesystem, so an early crash cannot write into the unbounded rootfs.

The daemon watches completed core writes with inotify and copies them into
/var/lib/hoki-crashdumps (0700 directory, 0600 files). It checks ELF program
segment extents to flag truncation from filesystem exhaustion or RLIMIT_CORE.
Processes must still be dumpable and have a nonzero core limit. Existing
sensorfwd allows unlimited cores; the buffer enforces the storage bound.
Android debuggerd can intercept its own processes' crashes; this does not
replace Android tombstones. fs.suid_dumpable remains 0.

For Qualcomm, the daemon opens all existing /dev/ramdump_* devices before
enabling the global full-ramdump switch. ADSP must be present. Mini dumps remain
disabled. The global switch affects other subsystems as well as ADSP; each
reader uses the same limits. No kernel panic/restart policy is changed.

Storage limits:
- At most 128 MiB per subsystem dump, 64 MiB per archived Linux core.
- At most four archived dumps, sharing 192 MiB including 4 KiB reserved metadata
  per dump. The fixed 64 MiB incoming filesystem brings the budget to 256 MiB.
  The incoming filesystem can also contain pending cores.
- Oldest collector-owned archives are removed before capture to reserve its
  worst-case size; existing sensor recordings are untouched.
- Requires 256 MiB free space; checks during copying too. Concurrent filesystem
  users can consume that reserve independently.
- 30-second read deadline, one storage writer. Overlapping subsystem captures
  are skipped. Size-limited or interrupted archives are partial ELF prefixes
  and may be unusable in ordinary debuggers.
- JSON metadata records byte count, boot ID, details and completion reason.
- A core that cannot be archived remains in the bounded incoming filesystem.
  Pending cores from earlier boots are retried at daemon start; pending files
  from this boot are preserved rather than risking copying a live kernel write.
  A full buffer prevents further cores until files are collected or removed.

The daemon blocks on device/inotify readiness while idle, with no periodic
poll. Stopping it disables the global full/mini switches and closes readers;
kernel ramdump.c releases pending captures on close, including truncation.
The core-buffer mount stays available across a collector restart.

Check:
```sh
systemctl status hoki-crashdump
cat /proc/sys/kernel/core_pattern
cat /sys/module/subsystem_restart/parameters/enable_ramdumps
ls -lh /var/lib/hoki-crashdumps
df -h /var/lib/hoki-core-buffer
```

Disable subsystem capture with `systemctl disable --now hoki-crashdump`.
To disable process cores too, remove the dedicated sysctl configuration and
restore the previous core pattern (`|/bin/false`). Preserve dumps before
unmounting or removing the buffer image. No kernel panic/pstore capture is added.

Run storage tests with `python3 test_crashdump.py`. On-watch smoke tests use a
disposable process; do not deliberately crash the DSP while recording.
