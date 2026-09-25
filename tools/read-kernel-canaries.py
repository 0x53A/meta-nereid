#!/usr/bin/env python3
"""Read Hoki kernel fix counters once; never mount debugfs or change watch state."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import shlex
import subprocess

FG_NAMES = (
    "ibatt_last_match_count", "ibatt_exhausted_count",
    "vbatt_last_match_count", "vbatt_exhausted_count",
    "esr_sw_cancelled_wake_count",
)
LPM_NAMES = (
    "idle_resched_aborts", "idle_entry_failures",
    "suspend_no_mode", "suspend_entry_failures",
)
COUNTERS = {"fg." + n: "/sys/kernel/debug/fg/" + n for n in FG_NAMES}
COUNTERS.update({"lpm." + n: "/sys/module/lpm_levels/parameters/" + n for n in LPM_NAMES})
PATHS = {
    "boot_id": "/proc/sys/kernel/random/boot_id",
    "uptime_start": "/proc/uptime",
    "kernel_release": "/proc/sys/kernel/osrelease",
    **COUNTERS,
    "suspend_stats": "/sys/kernel/debug/suspend_stats",
    "uptime_end": "/proc/uptime",
    "boot_id_end": "/proc/sys/kernel/random/boot_id",
}


def read_script():
    lines = []
    for name, path in PATHS.items():
        # Keys and paths are fixed constants, not caller-supplied shell text.
        lines += [
            "printf '%s\\n' " + shlex.quote("BEGIN " + name),
            "cat " + shlex.quote(path) + " 2>/dev/null || printf '%s\\n' UNAVAILABLE",
            "printf '\\n%s\\n' END",
        ]
    return "\n".join(lines) + "\n"


def parse_snapshot(raw):
    fields = {}
    name = None
    parts = []
    for line in raw.splitlines():
        if line.startswith("BEGIN "):
            if name is not None:
                raise ValueError("nested snapshot field")
            name, parts = line[6:], []
            if name not in PATHS or name in fields:
                raise ValueError("unexpected or duplicate snapshot field")
        elif line == "END":
            if name is None:
                raise ValueError("unexpected field end")
            fields[name] = "\n".join(parts).strip()
            name = None
        elif name is not None:
            parts.append(line)
        elif line.strip():
            raise ValueError("unexpected snapshot output")
    if name is not None or fields.keys() != PATHS.keys():
        raise ValueError("incomplete snapshot")
    def available(key):
        value = fields[key]
        return value if value and "UNAVAILABLE" not in value else None
    boot = available("boot_id")
    if not boot or boot != available("boot_id_end"):
        raise ValueError("boot identity unavailable or changed during read")
    counters = {}
    for key, path in COUNTERS.items():
        raw_value = available(key)
        try:
            value = int(raw_value) if raw_value is not None else None
        except ValueError:
            raise ValueError("invalid counter: " + key)
        counters[key] = {"value": value, "path": path,
                         "status": "available" if value is not None else "unavailable"}
    return {
        "schema": 1, "observed_at_utc": datetime.now(timezone.utc).isoformat(),
        "boot_id": boot, "kernel_release": available("kernel_release"),
        "uptime_start_seconds": float(fields["uptime_start"].split()[0]),
        "uptime_end_seconds": float(fields["uptime_end"].split()[0]),
        "counters": counters, "suspend_stats": available("suspend_stats"),
    }


def compare(previous, current):
    if previous.get("schema") != 1:
        raise ValueError("unsupported previous snapshot schema")
    if previous.get("boot_id") != current["boot_id"]:
        return {"status": "different_boot", "deltas": {}}
    if previous["uptime_end_seconds"] > current["uptime_start_seconds"]:
        return {"status": "overlapping_or_out_of_order", "deltas": {}}
    result = {}
    for key, item in current["counters"].items():
        before = previous.get("counters", {}).get(key, {}).get("value")
        after = item["value"]
        if before is None or after is None:
            result[key] = {"status": "unavailable", "delta": None}
        elif before < 0 or after < 0 or after < before:
            result[key] = {"status": "reset_or_wrap", "delta": None}
        else:
            result[key] = {"status": "comparable", "delta": after - before}
    return {"status": "same_boot", "deltas": result,
            "window_uptime_seconds": [previous["uptime_start_seconds"], current["uptime_end_seconds"]]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", help="SSH destination, e.g. root@hoki.local; omit to read locally")
    parser.add_argument("--host-key-alias", help="verify an existing SSH identity when connecting by another address")
    parser.add_argument("--previous", type=Path, help="compare with a prior JSON snapshot")
    parser.add_argument("--output", type=Path, help="write a new JSON file; defaults to stdout")
    args = parser.parse_args()
    command = ["sh", "-s"]
    if args.host:
        if args.host.startswith("-"):
            parser.error("host must not start with '-'")
        command = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
                   "-o", "StrictHostKeyChecking=yes"]
        if args.host_key_alias:
            command += ["-o", "HostKeyAlias=" + args.host_key_alias]
        command += [args.host, "sh -s"]
    read = subprocess.run(command, input=read_script(), text=True,
                          capture_output=True, timeout=30, check=True)
    snapshot = parse_snapshot(read.stdout)
    if args.previous:
        snapshot["comparison"] = compare(json.loads(args.previous.read_text()), snapshot)
    output = json.dumps(snapshot, indent=2) + "\n"
    if args.output:
        # Snapshots are evidence: do not silently overwrite one.
        with args.output.open("x") as stream:
            stream.write(output)
    else:
        print(output, end="")


if __name__ == "__main__":
    main()
