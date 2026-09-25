#!/usr/bin/env python3
"""Stage a bounded recording unit bundle. Never install or start services."""
import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re

UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z")


def path(value):
    if not isinstance(value, str) or not re.fullmatch(r"/[A-Za-z0-9_./-]+", value):
        raise ValueError("paths must be absolute and contain only simple unit-safe characters")
    if any(part in ("", ".", "..") for part in value.split("/")[1:]):
        raise ValueError("paths must be normalized")
    return value


def render(runtime, selected, recording_root, socket, power_loop, timezone, hal_limit=1073741824):
    if type(hal_limit) is not int or not 134217728 <= hal_limit <= 1073741824:
        raise ValueError("HAL budget must be 128 MiB through 1 GiB")
    owner = runtime.get("owner", "")
    boot = runtime.get("boot_id", "")
    if runtime.get("version") != 1 or not UUID.fullmatch(owner) or not UUID.fullmatch(boot):
        raise ValueError("invalid profile identity")
    if selected.get("boot_id") != boot:
        raise ValueError("endpoint discovery and profile boots differ")
    seconds = runtime.get("duration_seconds")
    if type(seconds) is not int or not 1 <= seconds <= 86400:
        raise ValueError("profile duration must be 1..86400 seconds")
    if type(timezone) is not int or not -43200 <= timezone <= 50400 or timezone % 3600:
        raise ValueError("stock clock sender currently requires a whole-hour timezone")
    controller, helper, profile = (path(runtime[key]) for key in ("controller", "helper", "session"))
    recording_root, socket, power_loop = map(path, (recording_root, socket, power_loop))
    if PurePosixPath(recording_root) == PurePosixPath(profile):
        raise ValueError("recording and profile directories must differ")
    apply = f"hoki-sleep-{owner}.service"
    restore = runtime.get("restore_unit", "")
    prefix = f"hoki-sleep-restore-{owner}-"
    if (runtime.get("apply_unit") != apply or not restore.startswith(prefix)
            or not restore.endswith(".service") or not UUID.fullmatch(restore[len(prefix):-8])):
        raise ValueError("invalid profile service identities")
    endpoints = selected.get("endpoints", {})
    for name in ("fsl_min", "fsl_sleep", "fsl_rhr", *(["fsl_wk"] if "fsl_wk" in endpoints else [])):
        if not re.fullmatch(r"09[0-9a-f]{16}11[0-9a-f]{16}", endpoints.get(name, "")):
            raise ValueError(f"missing or malformed {name} endpoint")
    target = f"hoki-recording-{owner}.target"
    hal = f"hoki-recording-hal-{owner}.service"
    ssc = f"hoki-recording-ssc-{owner}.service"
    gate = f"hoki-recording-ready-{owner}.service"
    power = f"hoki-recording-power-{owner}.service"
    # Profile expiry stops the target. These separate bounds cover other failures;
    # they do not promise the full duration after both recorders become ready.
    bound = seconds + 120
    units = {}
    units[target] = f"""[Unit]
Description=Bounded Hoki health recording session
BindsTo={apply} {hal} {ssc} {power}
After={apply} {hal} {ssc} {power}
"""
    units[gate] = f"""[Unit]
Description=Wait for verified sensor configuration
Requisite={apply}
After={apply}
PartOf={target}
[Service]
Type=oneshot
RemainAfterExit=yes
TimeoutStartSec=100
ExecStart={controller} --await-sleep {profile} {owner}
UMask=0077
"""
    shared = f"""[Unit]
Requires={gate}
After={gate}
PartOf={target}
[Service]
Type=notify
NotifyAccess=main
RuntimeMaxSec={bound}
UMask=0077
"""
    units[hal] = shared + f"""Environment=HOKI_HAL_LIMIT_BYTES={hal_limit}
TimeoutStartSec=30
TimeoutStopSec=40
ExecStart={controller} {socket} {recording_root}/hal 0
ExecStopPost={controller} --cleanup {socket} {recording_root}/hal
"""
    units[ssc] = shared + f"""TimeoutStartSec=70
TimeoutStopSec=15
Environment=LD_LIBRARY_PATH=/vendor/lib:/system/lib
Environment=SSC_TIME_OFFSET_SECONDS={timezone}
Environment=SSC_SLEEP_OBSERVE_SUID={endpoints['fsl_sleep']}
Environment=SSC_RHR_SUID={endpoints['fsl_rhr']}
Environment=SSC_JOURNAL_DIR={recording_root}/ssc
Environment=SSC_JOURNAL_LIMIT_BYTES=134217728
ExecStart={helper} --minute-record-clock {endpoints['fsl_min']}
ExecStopPost={helper} --cleanup
"""
    if "fsl_wk" in endpoints:
        units[ssc] += f"Environment=SSC_WORKOUT_SUID={endpoints['fsl_wk']}\n"
    units[power] = f"""[Unit]
Description=Session-owned recording suspend worker
BindsTo={hal} {ssc}
After={hal} {ssc}
PartOf={target}
[Service]
Type=exec
Environment=HOKI_POWER_SUPERVISOR={power}
ExecStart=/bin/sh {power_loop} {controller} {socket} {recording_root}/hal {hal} {ssc}
RuntimeMaxSec={bound}
TimeoutStopSec=5
KillMode=control-group
UMask=0077
"""
    units[f"{apply}.d/50-recording.conf"] = (
        f"[Unit]\nPartOf={target}\n[Service]\n"
        f"Environment=HOKI_HAL_LIMIT_BYTES={hal_limit}\n"
        f"ExecStartPre={controller} --check-recording-space {recording_root} {recording_root}/ssc\n"
    )
    units[f"{restore}.d/50-recording.conf"] = f"[Unit]\nAfter={hal} {ssc} {gate} {power}\n"
    manifest = dict(version=1, phase="prepared", owner=owner, boot_id=boot,
                    profile=runtime, selection=selected, recording_root=recording_root,
                    socket=socket, power_loop=power_loop, timezone_seconds=timezone,
                    hal_limit_bytes=hal_limit,
                    units=dict(target=target, hal=hal, ssc=ssc, gate=gate, power=power),
                    prerequisites=["current-boot discovery revalidation",
                        "private fresh recording root and SSC directory",
                        "healthy compatible sensorfw backend and control socket",
                        "loaded independently supervised profile recovery"],
                    workout_summary_enabled="fsl_wk" in endpoints,
                    archive_completion_verified=False)
    return units, manifest


def save_bundle(destination, units, manifest):
    destination.mkdir(mode=0o700)
    for name, data in {**units, "recording.json": json.dumps(manifest, indent=2) + "\n"}.items():
        output = destination / name
        if output.parent != destination:
            output.parent.mkdir(mode=0o700, exist_ok=True)
        fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    for directory in [*destination.glob("*.d"), destination, destination.parent]:
        fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runtime_json", type=Path)
    parser.add_argument("selection_json", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--recording-root", required=True)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--power-loop", required=True)
    parser.add_argument("--timezone-seconds", required=True, type=int)
    parser.add_argument("--hal-limit-bytes", type=int, default=1073741824)
    args = parser.parse_args()
    units, manifest = render(json.loads(args.runtime_json.read_text()),
                             json.loads(args.selection_json.read_text()),
                             args.recording_root, args.socket, args.power_loop,
                             args.timezone_seconds, args.hal_limit_bytes)
    save_bundle(args.destination, units, manifest)
