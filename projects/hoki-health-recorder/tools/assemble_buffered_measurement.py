#!/usr/bin/env python3
"""Assemble honest host-side measurement metadata from a frozen trial.

This offline tool pairs the first and last existing recorder checkpoint clock
samples with nearest supervisor battery JSONL rows. It does not contact or
change the watch. The two sample sources have distinct timestamps; their skew
is retained rather than presenting them as synchronized endpoints.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from metadata_json import decode_metadata

UUID = re.compile(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z')
CONTEXT_FIELDS = {'display_state', 'wifi_up', 'bluetooth_powered', 'usb_state'}


def read_json(path):
    data = path.read_bytes()
    if len(data) > 1_048_576:
        raise ValueError(f'oversized JSON document: {path}')
    value = decode_metadata(data)
    if not isinstance(value, dict):
        raise ValueError(f'expected a JSON object: {path}')
    return value


def finite(value, label):
    if type(value) not in (int, float) or not math.isfinite(value):
        raise ValueError(f'invalid {label}')
    return float(value)


def load_battery_rows(path):
    result = []
    for index, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip():
            continue
        row = decode_metadata(line.encode())
        if not isinstance(row, dict):
            raise ValueError(f'battery JSONL row {index} is not an object')
        stamp = finite(row.get('boottime_seconds'), f'battery row {index} BOOTTIME')
        counter = row.get('charge_counter')
        if type(counter) is int:
            counter_value = counter
        elif isinstance(counter, str) and re.fullmatch(r'-?[0-9]+', counter):
            counter_value = int(counter)
        else:
            raise ValueError(f'battery row {index} has invalid charge_counter')
        if stamp < 0:
            raise ValueError(f'battery row {index} has negative BOOTTIME')
        result.append((index, stamp, counter_value, row))
    if len(result) < 2:
        raise ValueError('battery.jsonl needs at least two counter samples')
    result.sort(key=lambda item: item[1])
    if any(left[1] >= right[1] for left, right in zip(result, result[1:])):
        raise ValueError('battery.jsonl has duplicate or regressing BOOTTIME')
    return result


def select_clock_window(controller):
    activation = finite(controller.get('activation_complete_boottime_seconds'),
                        'controller activation BOOTTIME')
    finish = finite(controller.get('end_boottime_seconds'), 'controller end BOOTTIME')
    if finish <= activation:
        raise ValueError('controller interval is empty or reversed')
    samples = controller.get('wake_held_samples')
    if not isinstance(samples, list):
        raise ValueError('controller has no checkpoint wake_held_samples')
    candidates = []
    for index, sample in enumerate(samples):
        if not isinstance(sample, dict):
            raise ValueError(f'wake_held_samples[{index}] is not an object')
        boot = finite(sample.get('boottime_seconds'), f'checkpoint {index} BOOTTIME')
        mono = finite(sample.get('monotonic_seconds'), f'checkpoint {index} MONOTONIC')
        if activation <= boot <= finish:
            candidates.append((index, boot, mono, sample))
    candidates.sort(key=lambda item: item[1])
    if len(candidates) < 2:
        raise ValueError('need at least two paired checkpoint clock samples inside controller interval')
    if any(a[1] >= b[1] or a[2] >= b[2] for a, b in zip(candidates, candidates[1:])):
        raise ValueError('checkpoint clocks duplicate or regress')
    first, last = candidates[0], candidates[-1]
    return activation, finish, first, last


def clock_endpoint(sample, side):
    index, boot, mono, row = sample
    return dict(boottime_seconds=boot, monotonic_seconds=mono,
                source=f'hal/controller.json:wake_held_samples[{index}]',
                checkpoint=row.get('checkpoint'), phase=row.get('phase'),
                wake_held=row.get('wake_held'), received=row.get('received'),
                durable=row.get('durable'), wake_error=row.get('wake_error'),
                boundary=side)


def nearest_counter(rows, clock_boot, side):
    index, stamp, counter, row = min(rows, key=lambda item: (abs(item[1]-clock_boot), item[1]))
    return dict(boottime_seconds=stamp, charge_counter_uah=counter,
                status=row.get('status'), capacity=row.get('capacity'),
                source=f'battery.jsonl:row[{index}]',
                skew_from_clock_seconds=stamp-clock_boot,
                nearest_to_clock_boundary=side)


def load_context(path):
    if path is None:
        return {}, None
    data = path.read_bytes()
    if len(data) > 1_048_576:
        raise ValueError('context JSON is oversized')
    value = decode_metadata(data)
    if not isinstance(value, dict):
        raise ValueError('context JSON must be an object')
    context = value.get('context', {})
    if not isinstance(context, dict):
        raise ValueError('context field must be an object')
    context = {key: context[key] for key in CONTEXT_FIELDS if key in context}
    for key in ('display_state', 'usb_state'):
        if key in context and not isinstance(context[key], str):
            raise ValueError(f'context.{key} must be a string')
    for key in ('wifi_up', 'bluetooth_powered'):
        if key in context and type(context[key]) is not bool:
            raise ValueError(f'context.{key} must be boolean')
    extra = {key: value[key] for key in ('powerd_status_start', 'powerd_status_end')
             if key in value}
    if any(not isinstance(item, dict) for item in extra.values()):
        raise ValueError('powerd status boundary values must be objects')
    return dict(context=context, **extra), hashlib.sha256(data).hexdigest()


def assemble(root, context_path):
    root = root.resolve(strict=True)
    session = read_json(root / 'session.json')
    controller = read_json(root / 'hal/controller.json')
    if (not UUID.fullmatch(session.get('id', '')) or
            not UUID.fullmatch(session.get('boot_id', '')) or
            not UUID.fullmatch(controller.get('session_id', '')) or
            controller.get('boot_id') != session.get('boot_id')):
        raise ValueError('session/controller identity mismatch')
    activation, finish, first, last = select_clock_window(controller)
    rows = load_battery_rows(root / 'battery.jsonl')
    start_clock = clock_endpoint(first, 'start')
    end_clock = clock_endpoint(last, 'end')
    counter_start = nearest_counter(rows, first[1], 'start')
    counter_end = nearest_counter(rows, last[1], 'end')
    if counter_start['source'] == counter_end['source']:
        raise ValueError('nearest battery rows resolve to one row; cannot form an endpoint slope')
    if counter_end['boottime_seconds'] <= counter_start['boottime_seconds']:
        raise ValueError('selected battery endpoint times do not increase')
    context_data, context_hash = load_context(context_path)
    result = dict(
        version=1,
        boot_id=session['boot_id'],
        session_id=session['id'],
        controller_session_id=controller['session_id'],
        profile=controller.get('collection_profile'),
        buffered_full_trial=controller.get('buffered_full_trial'),
        buffered_latency_cap_ns=controller.get('buffered_latency_cap_ns'),
        flush_interval_seconds=controller.get('flush_interval_seconds'),
        suspend_fallback_seconds=controller.get('suspend_fallback_seconds'),
        clock_scope='checkpoint_window',
        clock_source=('first and last paired CLOCK_BOOTTIME/CLOCK_MONOTONIC samples '
                      'already captured on recorder checkpoint polls; this window is '
                      'inside the controller interval, not whole-controller endpoints'),
        controller_interval=dict(activation_complete_boottime_seconds=activation,
                                 end_boottime_seconds=finish),
        start=start_clock,
        end=end_clock,
        counter_start=counter_start,
        counter_end=counter_end,
        counter_source=('nearest existing supervisor battery.jsonl row to each clock '
                        'boundary; BMS samples have their own BOOTTIME and signed '
                        'clock-boundary skew'),
        context=context_data.get('context', {}),
        context_source=(dict(name=context_path.name, sha256=context_hash)
                        if context_path is not None else None),
    )
    for field in ('powerd_status_start', 'powerd_status_end'):
        if field in context_data:
            result[field] = context_data[field]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trial', type=Path, help='frozen saved session directory')
    parser.add_argument('--output', type=Path, required=True,
                        help='new measurement.json path; existing files are refused')
    parser.add_argument('--context-json', type=Path,
                        help='optional host snapshot with context and powerd boundary status')
    args = parser.parse_args()
    try:
        result = assemble(args.trial, args.context_json)
        output = args.output
        if not output.is_absolute():
            output = args.trial / output
        if output.exists():
            raise ValueError(f'refusing to overwrite {output}')
        with output.open('x') as stream:
            json.dump(result, stream, indent=2, sort_keys=True, allow_nan=False)
            stream.write('\n')
        print(json.dumps(dict(output=str(output), clock_scope=result['clock_scope'],
                              start_boottime_seconds=result['start']['boottime_seconds'],
                              end_boottime_seconds=result['end']['boottime_seconds'],
                              counter_start_boottime_seconds=result['counter_start']['boottime_seconds'],
                              counter_end_boottime_seconds=result['counter_end']['boottime_seconds'],
                              counter_skew_seconds=[result['counter_start']['skew_from_clock_seconds'],
                                                    result['counter_end']['skew_from_clock_seconds']],
                              context_complete=CONTEXT_FIELDS <= result['context'].keys()),
                         indent=2))
    except (OSError, ValueError, TypeError) as error:
        parser.error(str(error))


if __name__ == '__main__':
    main()
