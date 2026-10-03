#!/usr/bin/env python3
"""Offline evidence summary; never promotes correlated events to proven wake causes."""
import argparse
import json
import math
from pathlib import Path
import re
import struct

RETURN = re.compile(r'RETURN elapsed=([\d.]+)s awake=([\d.]+)s suspended_estimate=([-\d.]+)s alarm_expired=(true|false)')


def read_json(directory, name):
    path = directory / name
    return json.loads(path.read_text()) if path.exists() else None


def events(text):
    output = []
    for line in text.splitlines():
        if not line or line.startswith('#'):
            continue
        fields = line.split()
        if len(fields) != 5 or not re.fullmatch('[0-9a-f]{128}', fields[4]):
            raise ValueError('malformed or truncated event record')
        arrival, source, handle, typ = map(int, fields[:4])
        output.append(dict(arrival_ns=arrival, source_ns=source, handle=handle, type=typ,
                           payload_hex=fields[4], delivery_age_ms=(arrival-source)/1e6))
    return output


def acceleration(records, handle):
    """HIDL type1 payload: XYZ float32 m/s² including gravity; preserve timestamps."""
    samples = []
    for event in records:
        if event['type'] != 1 or event['handle'] != handle:
            continue
        xyz = struct.unpack('<fff', bytes.fromhex(event['payload_hex'])[:12])
        if not all(math.isfinite(value) for value in xyz):
            raise ValueError('nonfinite raw accelerometer payload')
        samples.append(dict(source_ns=event['source_ns'], arrival_ns=event['arrival_ns'],
                            x=xyz[0], y=xyz[1], z=xyz[2]))
    return samples


def summarize(directory):
    config = read_json(directory, 'trial.json')
    selected = read_json(directory, 'selected.json')
    start = read_json(directory, 'window-start.json')
    end = read_json(directory, 'window-end.json')
    cleanup = read_json(directory, 'cleanup.json')
    finished = read_json(directory, 'finished.json')
    suspend_exit = read_json(directory, 'suspend-exit.json')
    logfile = directory / 'recorder.log'
    log = logfile.read_text() if logfile.exists() else ''
    records = events((directory / 'events.txt').read_text()) if (directory / 'events.txt').exists() else []
    matching = [e for e in records if selected and e['handle'] == selected['handle'] and e['type'] == selected['type']]
    in_window = [e for e in matching if start and end and start['boot_ns'] <= e['source_ns'] <= end['boot_ns']]
    suspend_file = directory / 'suspend.log'
    match = RETURN.search(suspend_file.read_text()) if suspend_file.exists() else None
    residency = float(match[3]) if match else None
    alarm = match[4] == 'true' if match else None
    clean = bool(cleanup and cleanup.get('restore_rc') == 0 and
                 cleanup.get('off_rc') == 0 and cleanup.get('sensorfwd_state') == 'active')
    complete = bool(finished and clean and (config['mode'] == 'baseline' or '\nEND events=' in '\n' + log))
    raw = None
    if config.get('raw_accel'):
        raw_selected = read_json(directory, 'raw-selected.json')
        samples = acceleration(records, raw_selected['handle']) if raw_selected else []
        gaps = [(b['source_ns'] - a['source_ns']) / 1e6 for a, b in zip(samples, samples[1:])]
        raw = dict(sample_count=len(samples), requested_batch_ms=config['accel_batch_ms'],
                   max_source_gap_ms=max(gaps, default=None),
                   nonincreasing_timestamps=sum(gap <= 0 for gap in gaps),
                   max_delivery_age_ms=max(((s['arrival_ns'] - s['source_ns']) / 1e6 for s in samples), default=None),
                   source_gaps_over_60ms=sum(gap > 60 for gap in gaps))
        complete = complete and bool(samples) and '\nEND events=' in '\n' + log
    if not complete:
        outcome = 'incomplete_trial'
    elif config['mode'] == 'awake':
        outcome = 'awake_events_only' if in_window else 'no_matching_awake_event'
    elif not match or not suspend_exit or suspend_exit['returncode'] != 0 or residency < 0.5:
        outcome = 'suspend_not_established'
    elif alarm:
        outcome = 'fallback_alarm_expired'
    elif config['mode'] == 'baseline':
        outcome = 'baseline_early_wake'
    elif in_window:
        outcome = 'early_wake_with_correlated_sensor_event'
    else:
        outcome = 'early_wake_without_matching_sensor_event'
    return dict(outcome=outcome, complete=complete, estimated_suspend_seconds=residency,
                alarm_expired=alarm, action=config['action'], mode=config['mode'],
                detector=selected, events=matching, source_window_event_count=len(in_window),
                rearm_count=log.count('REARM '), gesture_wake_proven=False, raw_accel=raw,
                limits=['Source window includes suspend entry overhead; timestamp clock equivalence requires validation.',
                        'Check wake-source deltas, kernel journal, fallback alarm and external action timing together.',
                        'When raw acceleration is enabled, batch delivery itself may cause a wake; an early return alone is not a gesture wake.',
                        'No event is a miss only when a deliberate action during verified suspend was independently observed.',
                        'This trial does not measure display-on latency or establish battery longevity.'])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    print(json.dumps(summarize(args.directory), indent=2))
