#!/usr/bin/env python3
"""Decode recovered health fields in a stable HOKISEN1 archive, offline only.

Firmware-specific software semantics, not physiological validation. See task0490.
Source-window membership does not establish clock equivalence or freshness.
"""
import argparse
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import struct

from verify_hal import HEADER, RECORD_SIZE, verify

SPO2 = 65561
BEAT_INTERVAL = 65574
ALGORITHM = {0: 'uninitialized', 1: 'idle', 2: 'waiting_for_no_motion',
             3: 'waiting_for_data', 4: 'running', 5: 'final'}
# These are enum codes from stock software, not independently composable flags.
SIGNAL = {0: 'no_errors', 1: 'no_signal', 2: 'low_snr', 4: 'low_perfusion',
          8: 'movement', 10: 'fit_too_tight', 20: 'fit_too_loose',
          40: 'out_of_bounds', -1: 'unknown'}


def safe_float(value):
    if math.isfinite(value):
        return value
    return 'nan' if math.isnan(value) else ('+inf' if value > 0 else '-inf')


def decode_health(typ, payload):
    if len(payload) != 64:
        raise ValueError('HAL payload must contain all 64 original bytes')
    if typ not in (SPO2, BEAT_INTERVAL):
        return None
    result = {'type': typ, 'raw_payload_hex': payload.hex()}
    if typ == BEAT_INTERVAL:
        value = struct.unpack_from('<f', payload)[0]
        result.update(kind='beat_interval', unit='ms', value=safe_float(value),
                      valid_numeric=math.isfinite(value) and value > 0)
        return result
    value, confidence, algorithm, signal = struct.unpack_from('<4f', payload)
    finite = all(math.isfinite(v) for v in (value, confidence, algorithm, signal))
    integral_states = finite and algorithm.is_integer() and signal.is_integer()
    algorithm_label = ALGORITHM.get(algorithm, 'unknown') if integral_states else 'unknown'
    signal_label = SIGNAL.get(signal, 'unknown') if integral_states else 'unknown'
    reasons = []
    if not finite:
        reasons.append('nonfinite_field')
    elif not integral_states:
        reasons.append('nonintegral_state')
    if algorithm_label == 'unknown':
        reasons.append('unknown_algorithm_state')
    if signal_label == 'unknown':
        reasons.append('unknown_signal_state')
    if algorithm != 5:
        reasons.append('not_final')
    if finite:
        if int(value) < 80:
            reasons.append('value_below_stock_service_threshold')
        if int(confidence) < 80:
            reasons.append('confidence_below_stock_default')
        if signal != 0:
            reasons.append('signal_not_clear')
    eligible = not reasons
    result.update(kind='spo2', value=safe_float(value), confidence=safe_float(confidence),
                  algorithm_state=safe_float(algorithm), signal_state=safe_float(signal),
                  algorithm_label=algorithm_label, signal_label=signal_label,
                  stock_service_eligible_default80=eligible,
                  stock_ui_eligible_default80=eligible and int(value) > 80,
                  reasons=reasons,
                  phase=('invalid' if not finite or not integral_states else
                         'unknown' if algorithm_label == 'unknown' else
                         'final' if algorithm == 5 else
                         'provisional' if algorithm == 4 else 'progress'))
    return result


def summarize(root, source_window_ns=None):
    root = Path(root)
    archive = verify(root, source_window_ns)
    counts = Counter()
    algorithms, signals, phases, reasons = (Counter() for _ in range(4))
    # Re-read the verified durable prefix and verify the full source hashes again.
    # Never include unacknowledged tail records in interpreted results.
    for file in archive['files']:
        path = root / file['name']
        digest = hashlib.sha256()
        with path.open('rb') as stream:
            header = stream.read(16)
            if header != HEADER:
                raise ValueError('source header changed after verification')
            digest.update(header)
            remaining = file['durable_bytes'] - 16
            while remaining:
                data = stream.read(min(remaining, RECORD_SIZE * 4096))
                if not data or len(data) % RECORD_SIZE:
                    raise ValueError('source truncated after verification')
                digest.update(data)
                remaining -= len(data)
                for arrival, timestamp, handle, typ, payload in struct.iter_unpack('<qqII64s', data):
                    if typ not in (SPO2, BEAT_INTERVAL):
                        continue
                    counts['health_records_total'] += 1
                    if source_window_ns and not source_window_ns[0] <= timestamp <= source_window_ns[1]:
                        counts['health_records_outside_window'] += 1
                        continue
                    row = decode_health(typ, payload)
                    counts[row['kind']] += 1
                    if typ == BEAT_INTERVAL:
                        counts['invalid_beat_interval_numeric'] += not row['valid_numeric']
                        continue
                    algorithms[str(row['algorithm_state'])] += 1
                    signals[str(row['signal_state'])] += 1
                    phases[row['phase']] += 1
                    reasons.update(row['reasons'])
                    counts['stock_service_eligible_default80'] += row['stock_service_eligible_default80']
                    counts['stock_ui_eligible_default80'] += row['stock_ui_eligible_default80']
            while data := stream.read(1024 * 1024):
                digest.update(data)
        if digest.hexdigest() != file['sha256']:
            raise ValueError(f'source changed after verification: {path.name}')
    return dict(source_window_ns=source_window_ns, counts=dict(counts),
                spo2_algorithm_states=dict(algorithms), spo2_signal_states=dict(signals),
                spo2_phases=dict(phases), spo2_rejection_reasons=dict(reasons),
                durable_records=archive['durable_records'],
                checkpoint_claim_and_bytes_consistent=archive['checkpoint_claim_and_bytes_consistent'],
                source_files=archive['files'],
                physiological_validity_verified=False, sensor_freshness_verified=False,
                interpretation='task0490 stock-software contract; eligible reports are not independent measurements')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--source-window-ns', type=int, nargs=2, metavar=('START', 'END'))
    args = parser.parse_args()
    print(json.dumps(summarize(args.capture, args.source_window_ns), indent=2, allow_nan=False))
