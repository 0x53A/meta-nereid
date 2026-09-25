#!/usr/bin/env python3
"""Compare selected HAL channels with saved records, including silent channels.

Use only a stable saved capture. Pairing metadata and events by directory is not
cryptographic provenance; preserve and compare source hashes independently.
"""
import argparse
import json
from pathlib import Path

from verify_hal import verify
from metadata_json import decode_metadata


def uint(value):
    if type(value) is not int or not 0 <= value <= 0xffffffff:
        raise ValueError('invalid HAL identifier/flags')
    return value


def nonnegative_integer(value, field):
    if type(value) is not int or value < 0:
        raise ValueError(f'invalid {field}')
    return value


def coverage(controller, archive):
    if type(controller.get('version')) is not int or controller['version'] != 1:
        raise ValueError('unsupported controller version')
    selected = {}
    handles = set()
    for item in controller['selected']:
        nonnegative_integer(item['period_ns'], 'requested period')
        nonnegative_integer(item['latency_ns'], 'requested latency')
        sensor = item['sensor']
        key = (uint(sensor['handle']), uint(sensor['type']))
        if not key[1] or key in selected or key[0] in handles:
            raise ValueError('ambiguous selected channel')
        handles.add(key[0])
        selected[key] = item
    activated_list = controller['activated_handles']
    activated = {uint(handle) for handle in activated_list}
    if len(activated) != len(activated_list) or not activated <= handles:
        raise ValueError('ambiguous or unselected activated handle')
    observed = {}
    metadata_records = 0
    for channel in archive['channels']:
        nonnegative_integer(channel['records'], 'archive record count')
        key = (uint(channel['handle']), uint(channel['type']))
        if not key[1]:
            metadata_records += channel['records']
            continue
        if key in observed:
            raise ValueError('duplicate archive channel')
        observed[key] = channel
    rows = []
    for key, item in sorted(selected.items()):
        sensor = item['sensor']
        mode = (uint(sensor['flags']) >> 1) & 7
        stats = observed.get(key)
        count = stats['records'] if stats else 0
        row = dict(handle=key[0], type=key[1], name=sensor['name'],
                   reporting_mode={0: 'continuous', 1: 'on_change', 2: 'one_shot',
                                   3: 'special_trigger'}.get(mode, 'unknown'),
                   activation_recorded=key[0] in activated,
                   requested_period_ns=item['period_ns'],
                   requested_latency_ns=item['latency_ns'],
                   records=count, output_observed=count > 0,
                   freshness_verified=False, continuity_verified=False)
        if stats:
            row['timestamp_statistics'] = {k: v for k, v in stats.items()
                                           if k not in ('handle', 'type', 'records')}
        # Only continuous reporting has a useful requested-period comparison.
        # A large interval is a diagnostic, not a count of lost samples.
        period = item['period_ns']
        interval = stats.get('max_interval_ns') if stats else None
        row['maximum_interval_requested_periods'] = (
            interval / period if mode == 0 and period > 0
            and type(interval) is int and interval >= 0 else None)
        row['missing_sample_count'] = None
        rows.append(row)
    return dict(version=1, scope='selected HAL channels only; SSC is separate',
                session_id=controller['session_id'], boot_id=controller['boot_id'],
                controller_phase=controller['phase'],
                checkpoint_claim_and_bytes_consistent=archive['checkpoint_claim_and_bytes_consistent'],
                controller_completion_verified=False,
                metadata_to_events_provenance_verified=False,
                source_window_ns=archive.get('source_window_ns'),
                selected_count=len(rows), activated_count=len(activated),
                selected_with_output=sum(row['output_observed'] for row in rows),
                selected_without_output=sum(not row['output_observed'] for row in rows),
                metadata_records=metadata_records, channels=rows,
                unexpected_channels=[observed[key] for key in sorted(observed) if key not in selected],
                interpretation='Activation and output do not prove fresh measurements. '
                'Silence may be normal for on-change/trigger sensors or charging optical sensors. '
                'Continuous-channel silence needs investigation; requested timing is not a delivery guarantee.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--source-window-ns', nargs=2, type=int, metavar=('START', 'END'),
                        help='additional timestamp statistics; does not change archive output counts')
    args = parser.parse_args()
    controller = decode_metadata((args.capture / 'controller.json').read_text())
    print(json.dumps(coverage(controller, verify(args.capture, args.source_window_ns)), indent=2))
