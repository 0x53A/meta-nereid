#!/usr/bin/env python3
"""Summarize recorder JSONL without printing coordinates; tolerate a torn final line."""
import argparse
import collections
import json
from pathlib import Path


def summarize(path):
    counts = collections.Counter()
    result = {"file": str(path), "events": 0, "fresh_position_signals": 0,
              "max_listed": 0, "max_used": 0, "max_snr": 0,
              "max_satellite_gap_ms": 0, "complete": False}
    previous_satellite = None
    last_boottime = None
    last_elapsed = None
    saw_satellite_report = False
    expected_sequence = 1
    with path.open('rb') as stream:
        for line_number, line in enumerate(stream, 1):
            try:
                row = json.loads(line)
            except (json.JSONDecodeError, UnicodeDecodeError):
                if not line.endswith(b'\n') and stream.read(1) == b'':
                    result['truncated_final_line'] = True
                    result['complete'] = False
                    break
                raise
            if not isinstance(row, dict):
                raise ValueError(f'record {line_number}: expected object')
            for field in ('sequence', 'boottime_ms', 'elapsed_ms'):
                value = row.get(field)
                minimum = 1 if field == 'sequence' else 0
                if type(value) is not int or value < minimum:
                    raise ValueError(f'record {line_number}: invalid {field}')
            if ((last_boottime is not None and row['boottime_ms'] < last_boottime)
                    or (last_elapsed is not None and row['elapsed_ms'] < last_elapsed)):
                raise ValueError(f'record {line_number}: regressing clock')
            if 'fresh_for_session' in row and type(row['fresh_for_session']) is not bool:
                raise ValueError(f'record {line_number}: invalid fresh_for_session')
            if row['sequence'] != expected_sequence:
                result['sequence_discontinuity'] = True
            expected_sequence = row['sequence'] + 1
            result['events'] += 1
            event = row['event']
            last_boottime = row['boottime_ms']
            last_elapsed = row['elapsed_ms']
            if event == 'session_start':
                previous_satellite = last_boottime
            counts[event + (':' + row['member'] if 'member' in row else '')] += 1
            result['elapsed_ms'] = row['elapsed_ms']
            result['complete'] = event == 'session_end'
            if event == 'session_end':
                result['end_reason'] = row['reason']
            if row.get('fresh_for_session'):
                result['fresh_position_signals'] += 1
            if event == 'dbus' and row.get('member') == 'SatelliteChanged':
                args = row.get('arguments')
                if (not isinstance(args, list) or len(args) != 5
                        or any(type(args[i]) is not int or args[i] < 0 for i in (1, 2))
                        or not isinstance(args[4], list)
                        or any(not isinstance(sat, list) or len(sat) != 4
                               or type(sat[3]) is not int for sat in args[4])):
                    raise ValueError(f'record {line_number}: invalid satellite statistics')
                saw_satellite_report = True
                result['max_used'] = max(result['max_used'], args[1])
                result['max_listed'] = max(result['max_listed'], args[2])
                result['max_snr'] = max([result['max_snr']] + [sat[3] for sat in args[4]])
                if previous_satellite is not None:
                    result['max_satellite_gap_ms'] = max(result['max_satellite_gap_ms'], row['boottime_ms'] - previous_satellite)
                previous_satellite = row['boottime_ms']
    if previous_satellite is not None and last_boottime is not None:
        tail_gap = last_boottime - previous_satellite
        result['last_satellite_age_ms'] = tail_gap if saw_satellite_report else None
        result['max_satellite_gap_ms'] = max(result['max_satellite_gap_ms'], tail_gap)
    result['event_counts'] = dict(sorted(counts.items()))
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('files', nargs='+', type=Path)
    args = parser.parse_args()
    print(json.dumps([summarize(path) for path in args.files], indent=2))
