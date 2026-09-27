#!/usr/bin/env python3
"""Audit saved buffered-recording trials or a staged latency sequence.

This is an offline host tool. It reads a preserved session directory and never
contacts or changes the watch. Suspend conclusions require paired clock
snapshots plus both powerd and kernel journals.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import statistics
import struct
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from metadata_json import decode_metadata
from verify_hal import HEADER, RECORD_SIZE, verify

UUID = re.compile(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z')
PPG = 65572
CONTINUOUS_MOTION = {1: 'accelerometer', 4: 'gyroscope'}
CONTEXT_FIELDS = {'display_state', 'wifi_up', 'bluetooth_powered', 'usb_state'}
WAKEUP_HEADER_FIELDS = (
    'name', 'active_count', 'event_count', 'wakeup_count', 'expire_count',
    'active_since', 'total_time', 'max_time', 'last_change', 'prevent_suspend_time')


def read_json(path):
    data = path.read_bytes()
    if len(data) > 1_048_576:
        raise ValueError(f'oversized JSON document: {path}')
    return decode_metadata(data)


def finite(value, name):
    if type(value) not in (int, float) or not math.isfinite(value):
        raise ValueError(f'invalid {name}')
    return float(value)


def powerd_max_sleep(status):
    if not isinstance(status, dict):
        return None
    config = status.get('config')
    value = config.get('max_sleep_seconds') if isinstance(config, dict) else None
    if value is None:
        value = status.get('max_sleep_seconds')
    return value if type(value) is int and value > 0 else None


def manifest_report(root, manifest, required=None):
    root = root.resolve(strict=True)
    manifest = manifest.resolve(strict=True)
    verified, entries = [], set()
    for line_number, line in enumerate(manifest.read_text().splitlines(), 1):
        if not line.strip():
            continue
        match = re.fullmatch(r'([0-9a-f]{64})\s+\*?(.+?)\s*', line)
        if not match:
            raise ValueError(f'invalid source manifest line {line_number}')
        expected, name = match.groups()
        name = name.strip()
        relative = Path(name.removeprefix('./'))
        if relative.is_absolute() or '..' in relative.parts:
            raise ValueError(f'unsafe source manifest path on line {line_number}')
        candidates = [root / relative, manifest.parent / relative]
        if relative.parts and relative.parts[0] == root.name:
            candidates.insert(0, root.joinpath(*relative.parts[1:]))
        target = next((candidate.resolve(strict=True) for candidate in candidates
                       if candidate.exists() and candidate.resolve().is_relative_to(root)), None)
        if target is None:
            # Whole-archive manifests may also name a sibling latest.json. Only
            # files beneath this session root are in scope for this report.
            if relative.parts and relative.parts[0] != root.name:
                continue
            raise ValueError(f'manifest entry is missing from trial: {name}')
        digest = hashlib.sha256()
        with target.open('rb') as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b''):
                digest.update(block)
        if digest.hexdigest() != expected:
            raise ValueError(f'source hash mismatch: {target.relative_to(root)}')
        rel = target.relative_to(root).as_posix()
        entries.add(rel)
        verified.append(rel)
    if required is None:
        required = {'session.json', 'battery.jsonl', 'hal/controller.json',
                    'hal/checkpoint.json'}
        required.update(p.relative_to(root).as_posix() for p in (root / 'hal').glob('events-*.bin'))
    else:
        required = set(required)
    missing = sorted(required - entries)
    if missing:
        raise ValueError(f'source manifest omits required trial files: {missing}')
    return dict(verified_files=len(verified), verified_paths=sorted(verified),
                all_capture_inputs_covered=True)


def read_jsonl(path):
    rows = []
    for index, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip():
            continue
        value = decode_metadata(line.encode())
        if not isinstance(value, dict):
            raise ValueError(f'non-object JSONL row {index}: {path}')
        rows.append(value)
    return rows


def battery_monitor_report(path, checkpoint_samples=(), measurement_start=None, measurement_end=None,
                           counter_boundary_times=()):
    rows = read_jsonl(path)
    points = []
    statuses = set()
    for index, row in enumerate(rows, 1):
        try:
            stamp = finite(row.get('boottime_seconds'), f'battery row {index} BOOTTIME')
            raw_counter = row.get('charge_counter')
            if type(raw_counter) is int:
                counter = raw_counter
            elif isinstance(raw_counter, str) and re.fullmatch(r'-?[0-9]+', raw_counter):
                counter = int(raw_counter)
            else:
                raise ValueError('invalid charge_counter')
        except (TypeError, ValueError) as error:
            raise ValueError(f'invalid battery row {index}: {error}') from error
        if stamp < 0:
            raise ValueError(f'invalid battery row {index}: negative BOOTTIME')
        status = row.get('status')
        if not isinstance(status, str) or not status:
            raise ValueError(f'invalid battery row {index}: status')
        points.append((stamp, counter, status))
        statuses.add(status)
    deltas = [b[0]-a[0] for a, b in zip(points, points[1:])]
    counters = [b[1]-a[1] for a, b in zip(points, points[1:])]
    checkpoint_times = [sample['boottime_seconds'] for sample in checkpoint_samples]
    endpoint_indices = {index for index, point in enumerate(points)
                        if any(abs(point[0]-stamp) <= 1e-6 for stamp in counter_boundary_times)}
    in_window = [(index, point) for index, point in enumerate(points)
                 if measurement_start is None or measurement_end is None or
                 measurement_start < point[0] < measurement_end]
    checkpoint_aligned = [(index, point) for index, point in in_window
                          if index not in endpoint_indices and
                          any(abs(point[0]-stamp) <= 1.0 for stamp in checkpoint_times)]
    off_checkpoint = [point[0] for index, point in in_window
                      if index not in endpoint_indices and
                      not any(abs(point[0]-stamp) <= 1.0 for stamp in checkpoint_times)]
    return dict(sample_count=len(points),
                observed_seconds=points[-1][0]-points[0][0] if len(points) > 1 else None,
                median_sample_interval_seconds=statistics.median(deltas) if deltas else None,
                maximum_sample_interval_seconds=max(deltas) if deltas else None,
                sampled_statuses=sorted(statuses),
                charge_counter_decreases=sum(delta < 0 for delta in counters),
                charge_counter_increases=sum(delta > 0 for delta in counters),
                samples_within_1s_of_existing_recorder_checkpoint=len(checkpoint_aligned),
                samples_outside_1s_of_existing_recorder_checkpoint=len(off_checkpoint),
                assigned_supervisor_endpoint_sample_count=len(endpoint_indices),
                assigned_supervisor_endpoint_sample_times=[points[index][0] for index in sorted(endpoint_indices)],
                possible_separate_timer_samples=off_checkpoint,
                mean_mA_from_periodic_log=None,
                interpretation=('supervisor battery telemetry; assigned endpoint rows feed the separate '
                                'charge-counter slope using their own BOOTTIME values, never as paired '
                                'suspend clocks; unassigned off-checkpoint rows may indicate extra '
                                'timed work and can confound residency'))


def read_wakeup_sources(path):
    lines = path.read_text().splitlines()
    if not lines:
        raise ValueError(f'empty wakeup_sources snapshot: {path}')
    header = lines[0].split()
    if header != list(WAKEUP_HEADER_FIELDS):
        raise ValueError(f'unrecognized wakeup_sources header: {path}')
    result = {}
    for line_number, line in enumerate(lines[1:], 2):
        if not line.strip():
            continue
        fields = line.split()
        if len(fields) != len(header):
            raise ValueError(f'invalid wakeup_sources row {line_number}: {path}')
        name = fields[0]
        values = {}
        for key, raw in zip(header[1:], fields[1:]):
            if raw != '-':
                try:
                    values[key] = int(raw)
                except ValueError as error:
                    raise ValueError(f'invalid {key} in {path}:{line_number}') from error
        if name in result:
            raise ValueError(f'duplicate wakeup source {name}: {path}')
        result[name] = values
    return result


def wakeup_source_delta(root, elapsed_seconds):
    before_path, after_path = root / 'wakeup-before.txt', root / 'wakeup-after.txt'
    if not before_path.exists() or not after_path.exists():
        return dict(available=False, reason='both boundary snapshots are required')
    before, after = read_wakeup_sources(before_path), read_wakeup_sources(after_path)
    rows = {}
    for name in sorted(set(before) | set(after)):
        if name not in before or name not in after:
            rows[name] = dict(complete_pair=False)
            continue
        a, b = before[name], after[name]
        metrics = {}
        for key in ('active_count', 'event_count', 'wakeup_count', 'expire_count',
                    'total_time', 'prevent_suspend_time'):
            if key in a and key in b:
                delta = b[key] - a[key]
                metrics[key + '_delta'] = delta
                if delta < 0:
                    raise ValueError(f'wakeup source counter regressed: {name}.{key}')
        if name == 'sensorfwd_recording' and 'prevent_suspend_time_delta' in metrics:
            ms = metrics['prevent_suspend_time_delta']
            metrics['prevent_suspend_fraction'] = (
                ms / (elapsed_seconds * 1000) if elapsed_seconds > 0 else None)
        rows[name] = dict(complete_pair=True, **metrics)
    sensor = rows.get('sensorfwd_recording')
    blockers = sorted(
        ((name, row.get('prevent_suspend_time_delta', 0), row.get('wakeup_count_delta', 0))
         for name, row in rows.items() if row.get('complete_pair') and
         row.get('prevent_suspend_time_delta', 0) > 0),
        key=lambda item: (-item[1], -item[2], item[0]))
    return dict(available=True, sensorfwd_recording=sensor,
                sources=rows,
                top_prevent_suspend_sources=[dict(name=name, prevent_suspend_time_ms=held,
                                                   wakeup_count=wakes)
                                             for name, held, wakes in blockers[:10]],
                interpretation='kernel wake-source counters; prevent_suspend_time is the direct occupancy field when present')


def monotonic_seconds(line):
    match = re.search(r'\[\s*(\d+(?:\.\d+)?)\]', line)
    return float(match.group(1)) if match else None


def journal_reports(root, start_mono, end_mono):
    power_path, kernel_path = root / 'powerd-journal.txt', root / 'kernel-journal.txt'
    power = None
    if power_path.exists():
        entries = []
        for line in power_path.read_text(errors='replace').splitlines():
            stamp = monotonic_seconds(line)
            match = re.search(r'\bsleep: residency ([0-9]+(?:\.[0-9]+)?)s\b', line)
            if stamp is not None and start_mono <= stamp <= end_mono and match:
                entries.append(dict(monotonic_seconds=stamp, residency_seconds=float(match.group(1))))
        power = dict(residency_count=len(entries),
                     measured_residency_seconds=sum(x['residency_seconds'] for x in entries),
                     entries=entries)
    kernel = None
    if kernel_path.exists():
        entries, pending, unmatched_exit = [], None, 0
        for line in kernel_path.read_text(errors='replace').splitlines():
            stamp = monotonic_seconds(line)
            if stamp is None or not start_mono <= stamp <= end_mono:
                continue
            entry = re.search(r'PM: suspend entry \((deep|s2idle)\)', line)
            if entry:
                if pending is not None:
                    entries.append(dict(state=pending['state'],
                                        entry_monotonic_seconds=pending['stamp'],
                                        exit_monotonic_seconds=None, paired=False))
                pending = dict(state=entry.group(1), stamp=stamp)
                continue
            if 'PM: suspend exit' in line:
                if pending is None:
                    unmatched_exit += 1
                else:
                    entries.append(dict(state=pending['state'],
                                        entry_monotonic_seconds=pending['stamp'],
                                        exit_monotonic_seconds=stamp,
                                        elapsed_monotonic_seconds=max(0.0, stamp-pending['stamp']),
                                        paired=True))
                    pending = None
        if pending is not None:
            entries.append(dict(state=pending['state'],
                                entry_monotonic_seconds=pending['stamp'],
                                exit_monotonic_seconds=None, paired=False))
        kernel = dict(entry_count=len(entries),
                      paired_deep_count=sum(x['state'] == 'deep' and x['paired'] for x in entries),
                      paired_s2idle_count=sum(x['state'] == 's2idle' and x['paired'] for x in entries),
                      unmatched_or_aborted_count=sum(not x['paired'] for x in entries),
                      unmatched_exit_count=unmatched_exit,
                      paired_deep_elapsed_monotonic_seconds=sum(
                          x.get('elapsed_monotonic_seconds', 0) for x in entries
                          if x['state'] == 'deep' and x['paired']),
                      paired_s2idle_elapsed_monotonic_seconds=sum(
                          x.get('elapsed_monotonic_seconds', 0) for x in entries
                          if x['state'] == 's2idle' and x['paired']),
                      entries=entries)
    return dict(powerd=power, kernel=kernel,
                suspend_journal_inputs_present=power is not None and kernel is not None,
                scope='journal counts are limited to the supplied files and measured monotonic window')


def wake_held_report(controller, start_boot, end_boot):
    rows = controller.get('wake_held_samples')
    if not isinstance(rows, list) or not rows:
        return dict(available=False,
                    reason='no piggybacked wake_held samples; final status is only a point observation')
    samples = []
    for index, row in enumerate(rows, 1):
        if not isinstance(row, dict) or type(row.get('wake_held')) is not bool:
            raise ValueError(f'invalid wake_held sample {index}')
        stamp = finite(row.get('boottime_seconds'), f'wake_held sample {index} BOOTTIME')
        mono = finite(row.get('monotonic_seconds'), f'wake_held sample {index} MONOTONIC')
        if not start_boot <= stamp <= end_boot:
            continue
        try:
            received, durable = int(row.get('received')), int(row.get('durable'))
        except (TypeError, ValueError) as error:
            raise ValueError(f'invalid counts in wake_held sample {index}') from error
        samples.append(dict(boottime_seconds=stamp, monotonic_seconds=mono,
                            wake_held=row['wake_held'],
                            received=received, durable=durable,
                            wake_error=row.get('wake_error'),
                            checkpoint=row.get('checkpoint'), phase=row.get('phase')))
    samples.sort(key=lambda item: item['boottime_seconds'])
    if any(a['boottime_seconds'] >= b['boottime_seconds'] for a, b in zip(samples, samples[1:])):
        raise ValueError('duplicate or regressing wake_held sample time')
    held_intervals = total = suspend_estimate = 0.0
    transitions = 0
    intervals = []
    for previous, current in zip(samples, samples[1:]):
        boot_delta = current['boottime_seconds']-previous['boottime_seconds']
        mono_delta = current['monotonic_seconds']-previous['monotonic_seconds']
        if mono_delta < -0.25:
            raise ValueError('wake_held sample MONOTONIC clock regressed')
        slept = max(0.0, boot_delta-mono_delta)
        duration = boot_delta
        total += duration
        suspend_estimate += slept
        if previous['wake_held']:
            held_intervals += duration
        transitions += previous['wake_held'] != current['wake_held']
        intervals.append(dict(start_boottime_seconds=previous['boottime_seconds'],
                              end_boottime_seconds=current['boottime_seconds'],
                              elapsed_boottime_seconds=boot_delta,
                              elapsed_monotonic_seconds=mono_delta,
                              suspend_from_clock_delta_seconds=slept,
                              wake_held_at_start=previous['wake_held'],
                              received_at_start=previous['received'],
                              durable_at_start=previous['durable']))
    return dict(available=bool(samples), sample_count=len(samples),
                held_samples=sum(sample['wake_held'] for sample in samples),
                transitions=transitions,
                sampled_interval_seconds=total,
                held_fraction_between_samples=(held_intervals/total if total > 0 else None),
                suspend_from_clock_delta_seconds_between_samples=suspend_estimate,
                suspend_fraction_between_samples=(suspend_estimate/total if total > 0 else None),
                every_sample_durable=(all(sample['received'] == sample['durable'] and
                                          sample['wake_error'] in (None, 0)
                                          for sample in samples)),
                samples=samples,
                intervals=intervals,
                interpretation='zero-order estimate between existing checkpoint samples only; no extra sampling timer')


def inspect_motion(stamps, period_ns, active_seconds):
    diffs = [b-a for a, b in zip(stamps, stamps[1:])]
    positive = [d for d in diffs if d > 0]
    median = statistics.median(positive) if positive else None
    threshold = max(1.5 * period_ns, 1.5 * median) if median is not None else None
    possible = sum(d > threshold for d in diffs) if threshold is not None else None
    period_gap_estimate = (sum(max(0, round(d / period_ns)-1) for d in positive)
                           if period_ns > 0 else None)
    expected_count = (max(1, round(active_seconds*1e9/period_ns))
                      if active_seconds > 0 and period_ns > 0 else None)
    return dict(records=len(stamps),
                nonpositive_intervals=sum(d <= 0 for d in diffs),
                median_interval_ms=median / 1e6 if median is not None else None,
                maximum_interval_ms=max(diffs) / 1e6 if diffs else None,
                possible_gap_threshold_ms=threshold / 1e6 if threshold is not None else None,
                intervals_above_possible_gap_threshold=possible,
                no_interval_exceeds_possible_gap_threshold=(possible == 0) if possible is not None else False,
                nominal_expected_record_count=expected_count,
                nominal_record_deficit=(max(0, expected_count-len(stamps))
                                        if expected_count is not None else None),
                estimated_missing_periods_from_intervals=period_gap_estimate,
                exact_missing_sample_count=None,
                loss_interpretation='nominal-period estimate only; no per-sample sequence number is available')


def inspect_ppg(rows, duration_seconds):
    gaps, intervals = [], []
    mode_switches = 0
    previous = None
    nonpositive = 0
    for stamp, arrival, mode in rows:
        if previous is not None:
            delta = stamp - previous[0]
            switched = mode != previous[2]
            if delta <= 0:
                nonpositive += 1
            elif delta > 100_000_000:
                gaps.append(dict(interval_ms=delta/1e6, mode_switched=switched,
                                 arrival_timestamp_equal=arrival == previous[1]))
            else:
                intervals.append(delta/1e6)
            mode_switches += switched
        previous = (stamp, arrival, mode)
    gap_ms = [row['interval_ms'] for row in gaps]
    return dict(records=len(rows),
                mode_switches=mode_switches,
                source_intervals_over_100ms=len(gaps),
                source_intervals_over_100ms_per_hour=(len(gaps)*3600/duration_seconds
                                                       if duration_seconds > 0 else None),
                gaps_at_mode_switch=sum(row['mode_switched'] for row in gaps),
                gaps_without_mode_switch=sum(not row['mode_switched'] for row in gaps),
                maximum_gap_ms=max(gap_ms) if gap_ms else None,
                median_normal_interval_ms=statistics.median(intervals) if intervals else None,
                nonpositive_intervals=nonpositive,
                gaps=gaps,
                meaning='mode/gap association only; PPG payload mode and gaps are not physiologically interpreted')


def trial_report(root, expected_profile=None, max_latency_ns=None, manifest_path=None):
    root = root.resolve(strict=True)
    session = read_json(root / 'session.json')
    controller = read_json(root / 'hal/controller.json')
    measurement = read_json(root / 'measurement.json')
    if (not UUID.fullmatch(session.get('boot_id', '')) or
            not UUID.fullmatch(session.get('id', '')) or
            not UUID.fullmatch(controller.get('session_id', '')) or
            controller.get('boot_id') != session.get('boot_id')):
        raise ValueError('session and HAL controller identities differ')
    if (type(measurement.get('version')) is not int or measurement.get('version') != 1 or
            measurement.get('boot_id') != session['boot_id'] or
            measurement.get('session_id') != session.get('id') or
            measurement.get('controller_session_id') != controller.get('session_id')):
        raise ValueError('measurement metadata belongs to a different boot or version')
    start, end = measurement.get('start'), measurement.get('end')
    if not isinstance(start, dict) or not isinstance(end, dict):
        raise ValueError('measurement requires start and end snapshots')
    start_boot = finite(start.get('boottime_seconds'), 'start BOOTTIME')
    end_boot = finite(end.get('boottime_seconds'), 'end BOOTTIME')
    start_mono = finite(start.get('monotonic_seconds'), 'start MONOTONIC')
    end_mono = finite(end.get('monotonic_seconds'), 'end MONOTONIC')
    elapsed_boot, elapsed_mono = end_boot-start_boot, end_mono-start_mono
    if elapsed_boot <= 0 or elapsed_mono <= 0:
        raise ValueError('nonpositive measurement interval')
    suspended = elapsed_boot-elapsed_mono
    if suspended < -0.25:
        raise ValueError('BOOTTIME elapsed is materially shorter than MONOTONIC')
    suspended = max(0.0, suspended)
    context = measurement.get('context')
    context_complete = isinstance(context, dict) and CONTEXT_FIELDS <= context.keys()

    if manifest_path is None:
        candidates = (root / 'watch-sha256.txt', root.parent / 'watch-sha256.txt')
        manifest_path = next((path for path in candidates if path.is_file()), candidates[0])
    watch_manifest = manifest_report(root, manifest_path)
    trial_manifest_path = root / 'trial-sha256.txt'
    evidence_required = {'session.json', 'battery.jsonl', 'measurement.json',
                         'hal/controller.json', 'hal/checkpoint.json'}
    evidence_required.update(p.relative_to(root).as_posix() for p in (root / 'hal').glob('events-*.bin'))
    for optional_name in ('powerd-journal.txt', 'kernel-journal.txt',
                          'wakeup-before.txt', 'wakeup-after.txt'):
        if (root / optional_name).is_file():
            evidence_required.add(optional_name)
    evidence_manifest = manifest_report(root, trial_manifest_path, evidence_required)
    activation = finite(controller.get('activation_complete_boottime_seconds'),
                        'activation complete BOOTTIME')
    finish = finite(controller.get('end_boottime_seconds'), 'controller end BOOTTIME')
    clock_scope = measurement.get('clock_scope', 'whole_controller')
    if clock_scope == 'whole_controller':
        if not start_boot <= activation <= finish <= end_boot:
            raise ValueError('measurement endpoints do not cover the controller capture interval')
    elif clock_scope == 'checkpoint_window':
        if not activation <= start_boot <= end_boot <= finish:
            raise ValueError('checkpoint clock samples fall outside the controller capture interval')
    else:
        raise ValueError('unknown measurement clock scope')
    archive = verify(root / 'hal', (round(activation*1e9), round(finish*1e9)))
    checkpoint = archive['checkpoint']
    final = controller.get('final_status')
    if not isinstance(final, dict):
        raise ValueError('missing controller final status')
    def count(value, label):
        if type(value) is int and value >= 0:
            return value
        if isinstance(value, str) and re.fullmatch(r'[0-9]+', value):
            return int(value)
        raise ValueError(f'invalid {label} count')

    final_counts = {key: count(final[key], key) for key in
                    ('received', 'submitted_records', 'durable_records')}
    durable_match = (final_counts['received'] == final_counts['submitted_records'] ==
                     final_counts['durable_records'] == archive['durable_records'] == checkpoint['records'])
    archive_structural_integrity = (checkpoint.get('complete') is True and
                                    checkpoint.get('final') is True and
                                    archive['durable_bytes'] == checkpoint['total_bytes'] and
                                    not archive['uncheckpointed_files'] and
                                    all(file['unacknowledged_tail_bytes'] == 0
                                        for file in archive['files']))
    loss_counts = dict(
        checkpoint_dropped=count(checkpoint['dropped'], 'checkpoint dropped'),
        checkpoint_input_failures=count(checkpoint['input_failures'], 'checkpoint input failures'),
        checkpoint_sequence_missing=count(checkpoint['sequence_missing'], 'checkpoint sequence missing'),
        final_dropped=count(final.get('dropped', -1), 'final dropped'),
        final_input_failures=count(final.get('input_failures', -1), 'final input failures'))
    producer_loss_free = all(value == 0 for value in loss_counts.values())
    control_plane_complete = (durable_match and controller.get('phase') == 'closed' and
                              final.get('stopped') is True and
                              final.get('checkpoint_complete') is True and
                              final.get('flush_failed') is False)
    archive_complete = archive_structural_integrity and control_plane_complete
    recovery_complete = (control_plane_complete and final.get('wake_held') is False and
                         final.get('wake_error') == 0)

    selected = {}
    latencies = set()
    for item in controller.get('selected', []):
        if not isinstance(item, dict) or not isinstance(item.get('sensor'), dict):
            raise ValueError('invalid selected sensor metadata')
        sensor = item.get('sensor', {})
        handle, typ = sensor.get('handle'), sensor.get('type')
        if type(handle) is not int or handle < 0 or type(typ) is not int or typ < 0:
            raise ValueError('invalid selected sensor handle/type')
        key = (handle, typ)
        if key in selected:
            raise ValueError(f'duplicate selected sensor handle/type: {key}')
        selected[key] = item
        latencies.add(int(item.get('latency_ns', -1)))
    buffered_requested = (controller.get('buffered_full_trial') is True or
                          measurement.get('buffered_full_trial') is True)
    profile = controller.get('collection_profile', measurement.get('profile'))
    cap_ns = controller.get('buffered_latency_cap_ns', measurement.get('buffered_latency_cap_ns'))
    if type(cap_ns) is not int or cap_ns <= 0:
        cap_ns = None

    wakeup_only = bool(selected) and all(
        type(item['sensor'].get('flags')) is int and item['sensor']['flags'] & 1 != 0
        for item in selected.values())
    fifo_assessment = []
    entry_plan_valid = bool(selected)
    for item in selected.values():
        latency = item.get('latency_ns')
        period = item.get('period_ns')
        sensor = item['sensor']
        reserved = item.get('fifo_reserved')
        if reserved is None:
            reserved = sensor.get('fifo_reserved')
        fifo_max = item.get('fifo_max')
        if fifo_max is None:
            fifo_max = sensor.get('fifo_max')
        if type(latency) is not int or latency < 0:
            entry_plan_valid = False
            fifo_assessment.append(dict(handle=sensor.get('handle'), type=sensor.get('type'),
                                        classification='invalid-latency-metadata'))
            continue
        if type(period) is not int or period <= 0 or (cap_ns is not None and latency > cap_ns):
            entry_plan_valid = False
        if latency == 0:
            classification = 'immediate-delivery'
            available_window_ns = None
        elif type(reserved) is not int or reserved <= 0:
            classification = 'unknown-reserve'
            available_window_ns = None
        else:
            available_window_ns = reserved * period if type(period) is int and period > 0 else None
            classification = ('within-advertised-reserve' if available_window_ns is not None and
                               latency <= available_window_ns else 'over-advertised-reserve')
        fifo_assessment.append(dict(handle=sensor.get('handle'), type=sensor.get('type'),
                                    period_ns=period, latency_ns=latency,
                                    fifo_reserved=reserved, fifo_max=fifo_max,
                                    advertised_window_ns=available_window_ns,
                                    classification=classification))
    positive_batch_count = sum(type(item.get('latency_ns')) is int and item['latency_ns'] > 0
                               for item in selected.values())
    profile_valid = profile == (expected_profile or 'full')
    flush_interval = controller.get('flush_interval_seconds', measurement.get('flush_interval_seconds'))
    fallback_interval = controller.get('suspend_fallback_seconds', measurement.get('suspend_fallback_seconds'))
    latency_cap_seconds = cap_ns / 1e9 if cap_ns is not None else None
    actual_cap_within_cli_bound = (latency_cap_seconds is not None and
                                   (max_latency_ns is None or cap_ns <= max_latency_ns))
    timing_policy_valid = (
        type(flush_interval) in (int, float) and math.isfinite(flush_interval) and flush_interval > 0 and
        type(fallback_interval) in (int, float) and math.isfinite(fallback_interval) and
        latency_cap_seconds is not None and flush_interval >= latency_cap_seconds and
        fallback_interval > latency_cap_seconds and fallback_interval <= 65 and
        abs(fallback_interval-(latency_cap_seconds+10)) <= 0.01)
    powerd_start = measurement.get('powerd_status_start', start.get('powerd_status'))
    powerd_end = measurement.get('powerd_status_end', end.get('powerd_status'))
    powerd_max_start = powerd_max_sleep(powerd_start)
    powerd_max_end = powerd_max_sleep(powerd_end)
    powerd_cap_covers_fallback = (
        powerd_max_start is not None and powerd_max_end is not None and
        type(fallback_interval) in (int, float) and
        powerd_max_start >= fallback_interval and powerd_max_end >= fallback_interval)
    if positive_batch_count == 0:
        entry_plan_valid = False
    if max_latency_ns is not None and cap_ns is not None and cap_ns > max_latency_ns:
        entry_plan_valid = False
    power_probe_configuration_valid = (buffered_requested and profile_valid and wakeup_only and
                                       entry_plan_valid and actual_cap_within_cli_bound and
                                       timing_policy_valid)
    fifo_risk_counts = {key: sum(row.get('classification') == key for row in fifo_assessment)
                        for key in ('over-advertised-reserve', 'unknown-reserve',
                                    'within-advertised-reserve', 'immediate-delivery')}

    selected_by_type = {}
    for (handle, typ), item in selected.items():
        selected_by_type.setdefault(typ, []).append((handle, item))
    source_start, source_end = round(activation*1e9), round(finish*1e9)
    active_seconds = max(0.0, finish-activation)
    streams = {(handle, typ): [] for handle, typ in selected}
    ppg_by_handle = {}
    for source in archive['files']:
        path = root / 'hal' / source['name']
        digest = hashlib.sha256()
        with path.open('rb') as stream:
            header = stream.read(16)
            if header != HEADER:
                raise ValueError(f'bad HOKISEN1 header: {path.name}')
            digest.update(header)
            remaining = source['durable_bytes']-16
            while remaining:
                block = stream.read(min(remaining, RECORD_SIZE*4096))
                if not block or len(block) % RECORD_SIZE:
                    raise ValueError(f'truncated HOKISEN1 segment: {path.name}')
                digest.update(block)
                for arrival, stamp, handle, typ, payload in struct.iter_unpack('<qqII64s', block):
                    if typ not in (*CONTINUOUS_MOTION, PPG) or not source_start <= stamp <= source_end:
                        continue
                    if (handle, typ) not in selected:
                        continue
                    streams[(handle, typ)].append(stamp)
                    if typ == PPG:
                        words = struct.unpack('<16I', payload)
                        ppg_by_handle.setdefault(handle, []).append(
                            (stamp, arrival, words[3] != 0 and words[6] != 0))
                remaining -= len(block)
            while tail := stream.read(1024 * 1024):
                digest.update(tail)
        if digest.hexdigest() != source['sha256']:
            raise ValueError(f'segment changed after verification: {path.name}')

    motion = {}
    for typ, label in CONTINUOUS_MOTION.items():
        rows = selected_by_type.get(typ, [])
        if not rows:
            motion[label] = dict(selected=False, no_interval_exceeds_possible_gap_threshold=False)
            continue
        if len(rows) > 1:
            # The report is conservative when firmware advertises duplicate
            # descriptors: each selected stream is audited separately.
            motion[label] = {
                'selected_streams': [dict(handle=handle, name=item['sensor'].get('name'),
                                          requested_period_ns=item.get('period_ns'),
                                          **inspect_motion(streams[(handle, typ)], int(item.get('period_ns', 0)), active_seconds))
                                    for handle, item in rows],
                'no_interval_exceeds_possible_gap_threshold': all(
                    inspect_motion(streams[(handle, typ)], int(item.get('period_ns', 0)), active_seconds)['no_interval_exceeds_possible_gap_threshold']
                    for _, item in rows)}
        else:
            handle, item = rows[0]
            motion[label] = dict(selected=True, handle=handle,
                                 sensor_name=item['sensor'].get('name'),
                                 requested_period_ns=item.get('period_ns'),
                                 **inspect_motion(streams[(handle, typ)], int(item.get('period_ns', 0)), active_seconds))

    ppg_streams = {str(handle): inspect_ppg(rows, elapsed_boot)
                   for handle, rows in sorted(ppg_by_handle.items())}
    ppg = dict(selected=bool(ppg_streams), streams=ppg_streams,
               meaning='mode/gap association only; PPG payload mode and gaps are not physiologically interpreted')
    counter_start_sample = measurement.get('counter_start', start)
    counter_end_sample = measurement.get('counter_end', end)
    if not isinstance(counter_start_sample, dict) or not isinstance(counter_end_sample, dict):
        raise ValueError('counter endpoint samples must be objects')
    charge_start = counter_start_sample.get('charge_counter_uah',
                                            counter_start_sample.get('charge_counter'))
    charge_end = counter_end_sample.get('charge_counter_uah',
                                        counter_end_sample.get('charge_counter'))
    counter_report = None
    def charge_counter(value, label):
        if type(value) is int:
            return value
        if isinstance(value, str) and re.fullmatch(r'-?[0-9]+', value):
            return int(value)
        if value is not None:
            raise ValueError(f'invalid {label} charge counter')
        return None

    charge_start = charge_counter(charge_start, 'start')
    charge_end = charge_counter(charge_end, 'end')
    if charge_start is not None and charge_end is not None:
        drop = charge_start-charge_end
        counter_start_boot = finite(counter_start_sample.get('boottime_seconds', start_boot),
                                    'counter start BOOTTIME')
        counter_end_boot = finite(counter_end_sample.get('boottime_seconds', end_boot),
                                  'counter end BOOTTIME')
        counter_elapsed = counter_end_boot-counter_start_boot
        valid_discharge = (drop >= 0 and
                           counter_start_sample.get('status') == 'Discharging' and
                           counter_end_sample.get('status') == 'Discharging' and
                           charge_start >= 0 and charge_end >= 0)
        counter_report = dict(start_uah=charge_start, end_uah=charge_end,
                              drop_uah=drop,
                              status_start=counter_start_sample.get('status'),
                              status_end=counter_end_sample.get('status'),
                              counter_start_boottime_seconds=counter_start_boot,
                              counter_end_boottime_seconds=counter_end_boot,
                              counter_elapsed_boottime_seconds=counter_elapsed,
                              counter_sample_sources=[counter_start_sample.get('source'),
                                                       counter_end_sample.get('source')],
                              counter_to_clock_skew_seconds=[
                                  counter_start_sample.get('skew_from_clock_seconds'),
                                  counter_end_sample.get('skew_from_clock_seconds')],
                              mean_mA=(drop*3.6/counter_elapsed
                                       if valid_discharge and counter_elapsed > 0 else None),
                              valid_discharge=valid_discharge,
                              basis='same BMS charge_counter; endpoint slope, not independent power measurement')

    journals = journal_reports(root, start_mono, end_mono)
    wake_sources = wakeup_source_delta(root, elapsed_boot)
    wake_held = wake_held_report(controller, start_boot, end_boot)
    target_latency = cap_ns
    return dict(version=1, boot_id=session['boot_id'], session_id=session.get('id'),
                profile=profile, buffered_full_trial=buffered_requested,
                requested_latency_cap_ns=target_latency,
                source_manifest=watch_manifest, evidence_manifest=evidence_manifest,
                measurement=dict(clock_scope=clock_scope,
                                 clock_source=measurement.get('clock_source'),
                                 start=start, end=end, elapsed_boottime_seconds=elapsed_boot,
                                 elapsed_monotonic_seconds=elapsed_mono,
                                 suspend_from_clock_delta_seconds=suspended,
                                 suspend_fraction=suspended/elapsed_boot,
                                 clock_counter_boundary_skew_seconds=(
                                     measurement.get('counter_start', {}).get('skew_from_clock_seconds'),
                                     measurement.get('counter_end', {}).get('skew_from_clock_seconds')),
                                 context=context,
                                 context_complete=context_complete,
                                 powerd_status_start=powerd_start,
                                 powerd_status_end=powerd_end),
                powerd_policy=dict(
                    max_sleep_seconds_start=powerd_max_start,
                    max_sleep_seconds_end=powerd_max_end,
                    recorder_fallback_seconds=fallback_interval,
                    max_sleep_covers_recorder_fallback=(
                        powerd_cap_covers_fallback if powerd_max_start is not None and
                        powerd_max_end is not None else None),
                    status_reason_start=(powerd_start.get('reason')
                                         if isinstance(powerd_start, dict) else None),
                    status_reason_end=(powerd_end.get('reason')
                                       if isinstance(powerd_end, dict) else None),
                    status_display_start=(powerd_start.get('display')
                                          if isinstance(powerd_start, dict) else None),
                    status_display_end=(powerd_end.get('display')
                                        if isinstance(powerd_end, dict) else None),
                    status_sleep_until_start=(powerd_start.get('sleep_until')
                                              if isinstance(powerd_start, dict) else None),
                    status_sleep_until_end=(powerd_end.get('sleep_until')
                                            if isinstance(powerd_end, dict) else None),
                    inhibitors_start=(powerd_start.get('inhibitors')
                                      if isinstance(powerd_start, dict) else None),
                    inhibitors_end=(powerd_end.get('inhibitors')
                                    if isinstance(powerd_end, dict) else None),
                    interpretation=('powerd max_sleep_seconds can clip a longer recorder '
                                    'fallback, causing earlier coordinator wakeups; both '
                                    'separately captured boundary status snapshots are needed '
                                    'to verify coverage')),
                controller=dict(phase=controller.get('phase'),
                                selected_count=len(selected),
                                verified_wakeup_delivery=controller.get('verified_wakeup_delivery'),
                                wakeup_only=wakeup_only,
                                fifo_assessment=fifo_assessment,
                                fifo_risk_counts=fifo_risk_counts,
                                positive_batch_count=positive_batch_count,
                                buffered_latency_cap_ns=cap_ns,
                                flush_interval_seconds=controller.get('flush_interval_seconds'),
                                suspend_fallback_seconds=controller.get('suspend_fallback_seconds'),
                                timing_policy_valid=timing_policy_valid,
                                selected_plan=controller.get('selected'),
                                buffered_metadata={key: controller.get(key) for key in (
                                    'buffered_full_trial', 'buffered_latency_cap_ns',
                                    'flush_interval_seconds', 'suspend_fallback_seconds')},
                                latency_values_ns=sorted(latencies),
                                power_probe_configuration_valid=power_probe_configuration_valid,
                                periodic_flushes=controller.get('periodic_flushes'),
                                final_wake_held=final.get('wake_held'),
                                final_wake_error=final.get('wake_error'),
                                final_counts=final_counts,
                                archive_durable_records=archive['durable_records'],
                                archive_complete=archive_complete,
                                structural_integrity=archive_structural_integrity,
                                control_plane_complete=control_plane_complete,
                                backend_recovery_complete=recovery_complete,
                                producer_loss_free=producer_loss_free,
                                producer_loss_counts=loss_counts,
                                checkpoint_counts={key: checkpoint.get(key) for key in (
                                    'records', 'dropped', 'input_failures', 'sequence_missing')},
                                checkpoint_complete=checkpoint.get('complete'),
                                checkpoint_final=checkpoint.get('final')),
                motion=motion, ppg=ppg, charge_counter=counter_report,
                battery_monitor=battery_monitor_report(
                    root / 'battery.jsonl', wake_held.get('samples', []), start_boot, end_boot,
                    [finite(counter_start_sample.get('boottime_seconds', start_boot),
                            'counter start BOOTTIME'),
                     finite(counter_end_sample.get('boottime_seconds', end_boot),
                            'counter end BOOTTIME')]),
                wakeup_sources=wake_sources, wake_held=wake_held, journals=journals,
                gates=dict(archive_integrity=archive_structural_integrity,
                           control_plane_complete=control_plane_complete,
                           backend_recovered=recovery_complete,
                           power_probe_configuration_valid=power_probe_configuration_valid,
                           powerd_sleep_ceiling_covers_fallback=(
                               powerd_cap_covers_fallback if powerd_max_start is not None and
                               powerd_max_end is not None else None),
                           source_manifest_verified=watch_manifest['all_capture_inputs_covered'],
                           evidence_manifest_verified=evidence_manifest['all_capture_inputs_covered']),
                sensor_loss=dict(producer_counters=loss_counts,
                                 producer_loss_free=producer_loss_free,
                                 motion=motion,
                                 ppg_gaps_by_handle=ppg_streams),
                evidence_availability=dict(
                    valid_counter_slope=bool(counter_report and counter_report['valid_discharge']),
                    suspend_journal_inputs_present=journals['suspend_journal_inputs_present'],
                    powerd_status_boundaries_available=(isinstance(powerd_start, dict) and
                                                        isinstance(powerd_end, dict)),
                    wake_source_context_available=(wake_sources['available'] and
                                                   isinstance(wake_sources.get('sensorfwd_recording'), dict) and
                                                   wake_sources['sensorfwd_recording'].get('complete_pair') is True),
                    sampled_wake_hold_available=wake_held['available']),
                limits=['The motion gap threshold is a screening rule, not an exact lost-sample count.',
                        'PPG gaps/mode associations do not establish optical accuracy or physiological validity.',
                        'Charge-counter endpoint slope is from the watch fuel gauge, not an independent power meter.'])


def compare_reports(reports):
    if len(reports) == 1:
        return reports[0]
    baseline = reports[0]
    base_measurement = baseline['measurement']
    comparisons = []
    same_boot_context = all(
        report['boot_id'] == baseline['boot_id'] and
        report['measurement'].get('context_complete') is True and
        report['measurement'].get('context') == base_measurement.get('context')
        for report in reports) and base_measurement.get('context_complete') is True
    ordered_nonoverlap = all(
        previous['measurement']['end']['monotonic_seconds'] <=
        current['measurement']['start']['monotonic_seconds']
        for previous, current in zip(reports, reports[1:]))
    caps = [report.get('requested_latency_cap_ns') for report in reports]
    caps_are_increasing = (all(type(cap) is int for cap in caps) and
                           all(previous < current for previous, current in zip(caps, caps[1:])))
    baseline_counter = baseline.get('charge_counter')
    baseline_held = baseline.get('wakeup_sources', {}).get('sensorfwd_recording')
    baseline_held_ms = (baseline_held.get('prevent_suspend_time_delta')
                        if isinstance(baseline_held, dict) else None)
    for report in reports[1:]:
        counter = report.get('charge_counter')
        held = report.get('wakeup_sources', {}).get('sensorfwd_recording')
        held_ms = held.get('prevent_suspend_time_delta') if isinstance(held, dict) else None
        current_ratio = None
        if (baseline_counter and counter and baseline_counter.get('valid_discharge') and
                counter.get('valid_discharge') and baseline_counter.get('mean_mA', 0) > 0):
            current_ratio = counter['mean_mA']/baseline_counter['mean_mA']
        comparisons.append(dict(
            reference_session=baseline.get('session_id'),
            session=report.get('session_id'),
            reference_latency_cap_ns=baseline.get('requested_latency_cap_ns'),
            latency_cap_ns=report.get('requested_latency_cap_ns'),
            reference_suspend_fraction=base_measurement.get('suspend_fraction'),
            suspend_fraction=report['measurement'].get('suspend_fraction'),
            suspend_fraction_delta=(report['measurement']['suspend_fraction']-
                                    base_measurement['suspend_fraction']),
            current_mA_ratio_to_first_rung=current_ratio,
            sensorfwd_prevent_suspend_ms_ratio_to_first_rung=(
                held_ms/baseline_held_ms if baseline_held_ms not in (None, 0) and
                held_ms is not None else None)))
    def summary(report):
        motion = report.get('motion', {})
        return dict(session=report.get('session_id'),
                    latency_cap_ns=report.get('requested_latency_cap_ns'),
                    latency_values_ns=report.get('controller', {}).get('latency_values_ns'),
                    flush_interval_seconds=report.get('controller', {}).get('flush_interval_seconds'),
                    suspend_fallback_seconds=report.get('controller', {}).get('suspend_fallback_seconds'),
                    producer_loss_counts=report.get('controller', {}).get('producer_loss_counts'),
                    accel=dict(records=motion.get('accelerometer', {}).get('records'),
                               nominal_deficit=motion.get('accelerometer', {}).get('nominal_record_deficit'),
                               interval_gap_estimate=motion.get('accelerometer', {}).get('estimated_missing_periods_from_intervals')),
                    gyro=dict(records=motion.get('gyroscope', {}).get('records'),
                              nominal_deficit=motion.get('gyroscope', {}).get('nominal_record_deficit'),
                              interval_gap_estimate=motion.get('gyroscope', {}).get('estimated_missing_periods_from_intervals')),
                    ppg={handle: dict(gaps=data.get('source_intervals_over_100ms'),
                                      gaps_at_mode_switch=data.get('gaps_at_mode_switch'),
                                      gaps_without_mode_switch=data.get('gaps_without_mode_switch'))
                         for handle, data in report.get('ppg', {}).get('streams', {}).items()},
                    elapsed_boottime_seconds=report.get('measurement', {}).get('elapsed_boottime_seconds'),
                    clock_scope=report.get('measurement', {}).get('clock_scope'),
                    suspend_from_clock_delta_seconds=report.get('measurement', {}).get('suspend_from_clock_delta_seconds'),
                    suspend_fraction=report.get('measurement', {}).get('suspend_fraction'),
                    powerd_measured_residency_seconds=report.get('journals', {}).get('powerd', {}).get('measured_residency_seconds')
                    if report.get('journals', {}).get('powerd') is not None else None,
                    powerd_policy=report.get('powerd_policy'),
                    kernel=report.get('journals', {}).get('kernel'),
                    wake_held=report.get('wake_held'),
                    top_prevent_suspend_sources=report.get('wakeup_sources', {}).get('top_prevent_suspend_sources'),
                    charge_counter=report.get('charge_counter'),
                    archive_integrity=report.get('gates', {}).get('archive_integrity'),
                    control_plane_complete=report.get('gates', {}).get('control_plane_complete'),
                    backend_recovered=report.get('gates', {}).get('backend_recovered'))

    return dict(version=1,
                staged_trial_sequence=dict(
                    sessions=[report.get('session_id') for report in reports],
                    latency_caps_ns=caps,
                    caps_strictly_increasing=caps_are_increasing,
                    same_boot_and_context=same_boot_context,
                    chronological_and_nonoverlapping=ordered_nonoverlap,
                    same_boot_context_and_order=same_boot_context and ordered_nonoverlap,
                    comparisons_to_first_rung=comparisons),
                rung_summaries=[summary(report) for report in reports],
                trials=reports,
                interpretation='Per-rung system behavior and sensor loss are descriptive. Same-boot/context order improves comparability but does not isolate recorder causality or establish long-run battery life.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trial', nargs='+', type=Path)
    parser.add_argument('--expected-profile',
                        help='require the controller to report this collection profile (default: full)')
    parser.add_argument('--max-latency-ms', type=float, default=40000,
                        help='maximum allowed buffered latency cap in ms (default: 40000)')
    parser.add_argument('--manifest', type=Path,
                        help='watch-side source checksum manifest (default: trial or parent directory)')
    args = parser.parse_args()
    if args.max_latency_ms <= 0 or not math.isfinite(args.max_latency_ms):
        parser.error('--max-latency-ms must be finite and positive')
    max_latency = round(args.max_latency_ms*1e6)
    reports = [trial_report(path, args.expected_profile, max_latency, args.manifest)
               for path in args.trial]
    print(json.dumps(compare_reports(reports), indent=2, allow_nan=False))


if __name__ == '__main__':
    main()
