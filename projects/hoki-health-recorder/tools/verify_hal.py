#!/usr/bin/env python3
"""Verify saved HOKISEN1 durable prefixes with bounded per-channel memory.

An optional inclusive source timestamp window adds a separate statistical view.
All durable records still participate in integrity checks and full-archive stats.
Window membership alone does not prove measurement freshness or clock identity.
"""
import argparse
import hashlib
import heapq
import json
import os
from pathlib import Path
import struct
from metadata_json import decode_metadata

HEADER = b'HOKISEN1' + struct.pack('<II', 88, 1)
RECORD_SIZE = 88
INTERVAL_EXAMPLES = 8


def integer(obj, key):
    value = obj[key]
    if type(value) is not int or value < 0:
        raise ValueError(f'invalid checkpoint counter {key}')
    return value


def new_channel(handle, typ):
    return dict(handle=handle, type=typ, records=0, nonincreasing=0,
                negative_age=0, interval_sum_ns=0, max_interval_ns=None,
                min_interval_ns=None, largest_source_intervals=[])


def add_record(row, arrival, timestamp, record_index):
    typ = row['type']
    if not row['records']:
        row.update(first_arrival_ns=arrival, first_timestamp_ns=timestamp,
                   max_age_ns=arrival-timestamp)
    if typ:
        row['max_age_ns'] = max(row['max_age_ns'], arrival-timestamp)
        row['negative_age'] += arrival < timestamp
        if row['records']:
            delta = timestamp - row['last_timestamp_ns']
            row['nonincreasing'] += delta <= 0
            row['interval_sum_ns'] += delta
            row['max_interval_ns'] = delta if row['max_interval_ns'] is None else max(row['max_interval_ns'], delta)
            row['min_interval_ns'] = delta if row['min_interval_ns'] is None else min(row['min_interval_ns'], delta)
            if delta > 0:
                example = (delta, row['last_timestamp_ns'], timestamp,
                           row['last_arrival_ns'], arrival,
                           row['last_record_index'], record_index)
                heap = row['largest_source_intervals']
                if len(heap) < INTERVAL_EXAMPLES:
                    heapq.heappush(heap, example)
                else:
                    heapq.heappushpop(heap, example)
    row['last_arrival_ns'], row['last_timestamp_ns'] = arrival, timestamp
    row['last_record_index'] = record_index
    row['records'] += 1


def finish_channel(row):
    row.pop('last_record_index', None)
    row['largest_source_intervals'] = [dict(zip(
        ('interval_ns', 'previous_timestamp_ns', 'timestamp_ns',
         'previous_arrival_ns', 'arrival_ns', 'previous_record_index', 'record_index'),
        example)) for example in sorted(row['largest_source_intervals'], reverse=True)]
    if row['type'] and row['records'] > 1:
        row['mean_interval_ns'] = row['interval_sum_ns'] / (row['records'] - 1)
    if not row['type']:
        for key in ('nonincreasing', 'negative_age', 'interval_sum_ns', 'max_interval_ns', 'min_interval_ns', 'max_age_ns', 'largest_source_intervals'):
            del row[key]


def verify(root, source_window_ns=None):
    if source_window_ns is not None:
        if (not isinstance(source_window_ns, (tuple, list))
                or len(source_window_ns) != 2
                or any(type(n) is not int or n < 0 for n in source_window_ns)
                or source_window_ns[0] > source_window_ns[1]):
            raise ValueError('invalid inclusive source timestamp window')
    root = Path(root)
    checkpoint = decode_metadata((root / 'checkpoint.json').read_text())
    if integer(checkpoint, 'version') != 1:
        raise ValueError('unsupported checkpoint version')
    for key in ('records', 'total_bytes', 'segment_bytes', 'dropped', 'input_failures', 'sequence_missing'):
        integer(checkpoint, key)
    segment = integer(checkpoint, 'segment')
    if segment > 999999:
        raise ValueError('segment outside six-digit namespace')
    paths = sorted(root.glob('events-*.bin'))
    if len(paths) < segment + 1:
        raise ValueError('missing checkpointed segments')
    expected = {f'events-{index:06d}.bin' for index in range(segment + 1)}
    channels, files = {}, []
    records = total = 0
    for index in range(segment + 1):
        path = root / f'events-{index:06d}.bin'
        with path.open('rb') as stream:
            before = os.fstat(stream.fileno())
            limit = integer(checkpoint, 'segment_bytes') if index == segment else before.st_size
            if limit < 16 or limit > before.st_size or (limit - 16) % RECORD_SIZE:
                raise ValueError(f'invalid durable length: {path.name}')
            header = stream.read(16)
            if header != HEADER:
                raise ValueError(f'invalid segment header: {path.name}')
            digest = hashlib.sha256(header)
            remaining = limit - 16
            while remaining:
                data = stream.read(min(remaining, RECORD_SIZE * 4096))
                if not data or len(data) % RECORD_SIZE:
                    raise ValueError(f'truncated durable records: {path.name}')
                digest.update(data)
                remaining -= len(data)
                for offset in range(0, len(data), RECORD_SIZE):
                    arrival, timestamp, handle, typ = struct.unpack_from('<qqII', data, offset)
                    key = (handle, typ)
                    if key not in channels:
                        # Real inventories are small. Corrupt handles/types must
                        # not turn a bounded-memory pass into unbounded storage.
                        if len(channels) >= 4096:
                            raise ValueError('excessive distinct channels')
                        channels[key] = new_channel(handle, typ)
                        if source_window_ns is not None and typ:
                            channels[key]['source_window_statistics'] = new_channel(handle, typ)
                            channels[key]['source_before_window'] = 0
                            channels[key]['source_after_window'] = 0
                    row = channels[key]
                    add_record(row, arrival, timestamp, records)
                    if source_window_ns is not None and typ:
                        start, end = source_window_ns
                        if timestamp < start:
                            row['source_before_window'] += 1
                        elif timestamp > end:
                            row['source_after_window'] += 1
                        else:
                            add_record(row['source_window_statistics'], arrival, timestamp, records)
                    records += 1
            tail = 0
            while data := stream.read(1024 * 1024):
                digest.update(data)
                tail += len(data)
            after = os.fstat(stream.fileno())
            current = path.stat()
            identity = lambda st: (st.st_size, st.st_mtime_ns, st.st_ctime_ns, st.st_ino, st.st_dev)
            if identity(before) != identity(after) or identity(after) != identity(current) or limit + tail != before.st_size:
                raise ValueError(f'file changed during verification: {path.name}')
            files.append(dict(name=path.name, bytes=before.st_size, durable_bytes=limit,
                              unacknowledged_tail_bytes=tail, sha256=digest.hexdigest()))
            total += limit
    if records != integer(checkpoint, 'records') or total != integer(checkpoint, 'total_bytes'):
        raise ValueError('checkpoint totals disagree with durable bytes')
    uncheckpointed = [dict(name=p.name, bytes=p.stat().st_size) for p in paths if p.name not in expected]
    clean = (checkpoint.get('complete') is True and checkpoint.get('final') is True
             and all(integer(checkpoint, key) == 0 for key in ('dropped', 'input_failures', 'sequence_missing'))
             and not uncheckpointed and not any(f['unacknowledged_tail_bytes'] for f in files))
    for row in channels.values():
        finish_channel(row)
        if 'source_window_statistics' in row:
            finish_channel(row['source_window_statistics'])
    return dict(checkpoint=checkpoint, durable_records=records, durable_bytes=total,
                checkpoint_claim_and_bytes_consistent=clean,
                controller_completion_verified=False, sensor_freshness_verified=False,
                source_timestamp_domain_verified=False,
                source_window_ns=source_window_ns,
                files=files, uncheckpointed_files=uncheckpointed,
                channels=[channels[key] for key in sorted(channels)])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--source-window-ns', nargs=2, type=int, metavar=('START', 'END'),
                        help='additional statistics for inclusive source timestamps; same clock domain required')
    args = parser.parse_args()
    print(json.dumps(verify(args.capture, args.source_window_ns), indent=2))
