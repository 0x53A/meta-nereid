#!/usr/bin/env python3
"""Summarize saved suspend evidence and battery samples, without lifetime claims.

Battery CSV rows require boottime, capacity and a nonempty status. Malformed or
incomplete rows fail validation rather than being silently omitted. Missing
optional display_blank values are reported as unknown.
"""
import argparse
import csv
import json
import math
from pathlib import Path
import re
from metadata_json import decode_metadata

UUID = r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}'
NAME = re.compile(rf'suspend-({UUID})-(intent|result)\.json\Z')


def number(value):
    if type(value) not in (int, float):
        raise ValueError('non-finite or nonnumeric timing')
    try:
        value = float(value)
    except OverflowError as error:
        raise ValueError('timing outside floating-point range') from error
    if not math.isfinite(value):
        raise ValueError('non-finite or nonnumeric timing')
    return value


def read_json(path):
    with path.open('rb') as stream:
        data = stream.read(131073)
    if len(data) > 131072:
        raise ValueError('oversized metadata')
    return decode_metadata(data)


def suspend_report(controller, documents):
    boot, session = controller['boot_id'], controller['session_id']
    if type(controller.get('version')) is not int or controller['version'] != 1 or any(
            not isinstance(v, str) or not re.fullmatch(UUID, v) for v in (boot, session)):
        raise ValueError('invalid controller identity')
    attempts = {}
    for filename, value in documents:
        match = NAME.fullmatch(filename)
        if not match:
            raise ValueError('invalid suspend evidence filename')
        identity, kind = match.groups()
        if (type(value.get('version')) is not int or value['version'] != 1 or value.get('boot_id') != boot
                or value.get('session_id') != session):
            raise ValueError('suspend evidence belongs to a different recording')
        entry = attempts.setdefault(identity, {})
        if kind in entry:
            raise ValueError('duplicate suspend evidence')
        entry[kind] = value
    elapsed = awake = suspended = 0.0
    completed = expired = skipped = 0
    missing = []
    failures = []
    for identity, evidence in attempts.items():
        if 'intent' not in evidence:
            raise ValueError('result without durable intent')
        intent = evidence['intent']
        if not re.fullmatch(rf'hoki-recording-suspend-{UUID}\.service',
                            intent.get('supervisor', '')):
            raise ValueError('invalid suspend supervisor')
        if number(intent['boottime_seconds']) < 0:
            raise ValueError('negative intent time')
        if type(intent['alarm_seconds']) is not int or not 3 <= intent['alarm_seconds'] <= 20:
            raise ValueError('invalid alarm interval')
        if 'result' not in evidence:
            missing.append(identity)
            continue
        result = evidence['result']
        boundaries = ('start_boottime_seconds', 'end_boottime_seconds')
        if any(key in result for key in boundaries):
            if result.get('result') != 'returned' or not all(key in result for key in boundaries):
                raise ValueError('incomplete or non-returned suspend measurement boundaries')
            start, end = (number(result[key]) for key in boundaries)
            if (start < number(intent['boottime_seconds']) or end < start
                    or abs(number(end - start) - number(result['elapsed_seconds'])) > 0.000001):
                raise ValueError('inconsistent suspend measurement boundaries')
        if result.get('result') == 'failed':
            errno = result.get('errno')
            stage = result.get('stage')
            retryable = (stage, errno) in [('mem_write', 16), ('wakeup_count_commit', 22)]
            if (stage not in ('mem_write', 'wakeup_count_commit')
                    or result.get('suspend_requested') is not (stage == 'mem_write')
                    or (errno is not None and (type(errno) is not int or not 1 <= errno <= 4095))
                    or 'errno' not in result
                    or ('retryable' in result and result['retryable'] is not retryable)
                    or any(key in result for key in ('elapsed_seconds', 'awake_seconds',
                                                     'suspended_estimate_seconds', 'alarm_expired'))):
                raise ValueError('invalid failed suspend result')
            failures.append(dict(attempt=identity, stage=stage, errno=errno))
            continue
        if result.get('result') == 'skipped':
            if (result.get('suspend_requested') is not False
                    or result.get('reason') != 'recording_pending_durability'
                    or any(key in result for key in ('elapsed_seconds', 'awake_seconds',
                                                     'suspended_estimate_seconds', 'alarm_expired'))):
                raise ValueError('invalid skipped suspend result')
            skipped += 1
            continue
        e, a, s = (number(result[key]) for key in (
            'elapsed_seconds', 'awake_seconds', 'suspended_estimate_seconds'))
        # The two clocks are sampled sequentially; allow sub-millisecond skew.
        if (result.get('result') != 'returned' or type(result.get('alarm_expired')) is not bool
                or e < 0 or a < 0 or s < -0.001 or abs((e - a) - s) > 0.000001):
            raise ValueError('inconsistent suspend clock evidence')
        elapsed = number(elapsed + e)
        awake = number(awake + a)
        suspended = number(suspended + max(0, s))
        completed += 1
        expired += result['alarm_expired']
    return dict(intent_count=len(attempts), returned_count=completed,
                failed_mem_write_count=sum(x['stage'] == 'mem_write' for x in failures),
                failed_mem_writes=sorted((x for x in failures if x['stage'] == 'mem_write'),
                                        key=lambda item: item['attempt']),
                failed_wakeup_count_commit_count=sum(x['stage'] == 'wakeup_count_commit' for x in failures),
                failed_attempts=sorted(failures, key=lambda item: item['attempt']),
                skipped_before_suspend_count=skipped,
                intents_without_result=sorted(missing), alarm_expired_count=expired,
                measured_call_elapsed_seconds=elapsed, measured_call_awake_seconds=awake,
                measured_call_suspended_estimate_seconds=suspended,
                scope='returned mem-write calls only; excludes time between attempts',
                alarm_was_wake_cause_verified=False, whole_session_suspend_fraction=None,
                recorder_continuity_verified=False, complete_session_verified=False)


def battery_number(value, row_number, field):
    try:
        if type(value) is bool:
            raise ValueError('boolean is not a numeric sample')
        return number(float(value))
    except (TypeError, ValueError, OverflowError) as error:
        raise ValueError(f'invalid battery sample {row_number}: {field}') from error


def battery_report(rows):
    first = last = None
    statuses = set()
    display = set()
    count = 0
    max_gap = 0.0
    for row_number, row in enumerate(rows, 1):
        # DictReader uses a None key for fields beyond the header. Do not let a
        # shifted or torn row silently contribute to a measurement summary.
        if None in row:
            raise ValueError(f'invalid battery sample {row_number}: extra columns')
        time = battery_number(row.get('boottime'), row_number, 'boottime')
        capacity = battery_number(row.get('capacity'), row_number, 'capacity')
        status = row.get('status')
        if not isinstance(status, str) or not status.strip():
            raise ValueError(f'invalid battery sample {row_number}: status')
        if time < 0 or not 0 <= capacity <= 100:
            raise ValueError('invalid battery sample')
        if last is not None:
            if time <= last[0]:
                raise ValueError('regressing or duplicate battery sample time')
            max_gap = max(max_gap, time - last[0])
        last = (time, capacity)
        if first is None:
            first = last
        statuses.add(status)
        display.add(row.get('display_blank') or 'unknown')
        count += 1
    return dict(samples=count, observed_seconds=last[0] - first[0] if count else None,
                capacity_start_percent=first[1] if count else None,
                capacity_end_percent=last[1] if count else None,
                net_capacity_drop_percentage_points=first[1] - last[1] if count else None,
                maximum_sample_gap_seconds=max_gap if count > 1 else None,
                sampled_statuses=sorted(statuses), sampled_display_blank_values=sorted(display),
                all_samples_discharging=statuses == {'Discharging'},
                continuous_discharge_verified=False, integrated_energy_wh=None,
                estimated_battery_life_hours=None,
                limitation='sparse percentage samples; no charge integration or lifetime extrapolation')


def battery_csv_report(stream):
    # Validate before DictReader can collapse duplicate names into one value,
    # including when the file contains a header but no samples yet.
    try:
        rows = csv.DictReader(stream, strict=True)
        fields = rows.fieldnames
        if (not fields or any(not field.strip() for field in fields)
                or len(set(fields)) != len(fields)
                or not {'boottime', 'capacity', 'status'}.issubset(fields)):
            raise ValueError('invalid battery CSV header')
        return battery_report(rows)
    except csv.Error as error:
        raise ValueError(f'invalid battery CSV: {error}') from error


def analyze(root):
    hal = root / 'recording/hal'
    controller = read_json(hal / 'controller.json')
    files = sorted(hal.glob('suspend-*.json'))
    if len(files) > 100000:
        raise ValueError('too many suspend metadata files')
    report = dict(version=1, suspend=suspend_report(
        controller, ((p.name, read_json(p)) for p in files)))
    telemetry = root / 'telemetry.csv'
    if telemetry.exists():
        with telemetry.open(newline='') as stream:
            report['battery'] = battery_csv_report(stream)
    else:
        report['battery'] = None
    report['source_hashes_verified'] = False
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('saved_capture_root', type=Path)
    args = parser.parse_args()
    print(json.dumps(analyze(args.saved_capture_root), indent=2, allow_nan=False))
