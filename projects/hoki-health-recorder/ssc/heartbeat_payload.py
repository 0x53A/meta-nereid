"""Offline decoding of stock heartbeat payloads; no sensor requests are sent.

Timestamp units and physiological interpretation are deliberately unspecified.
The caller must independently verify the SSC source, envelope and durable journal.
"""
import argparse
import json


def decode(event_id, payload):
    if event_id not in (767, 1028, 1029):
        raise ValueError('unsupported heartbeat event')
    if len(payload) > 4096:
        raise ValueError('heartbeat payload exceeds decoder bound')
    pos = 0

    def varint():
        nonlocal pos
        value = 0
        for shift in range(0, 70, 7):
            if pos >= len(payload):
                raise ValueError('truncated varint')
            byte = payload[pos]
            pos += 1
            if shift == 63 and byte > 1:
                raise ValueError('uint64 overflow')
            value |= (byte & 127) << shift
            if byte < 128:
                return value
        raise ValueError('oversized varint')

    names = {1: 'unix_ts_sec', 2: 'force_stop'} if event_id == 767 else {
        1: 'timestamp', 2: 'ppg', 3: 'quality'}
    values, unknown = {}, []
    while pos < len(payload):
        start = pos
        key = varint()
        number, wire = key >> 3, key & 7
        if not 0 < number < (1 << 29):
            raise ValueError('invalid field number')
        if wire == 0:
            value = varint()
        elif wire in (1, 2, 5):
            size = varint() if wire == 2 else (8 if wire == 1 else 4)
            if size > len(payload) - pos:
                raise ValueError('truncated field')
            value = payload[pos:pos+size]
            pos += size
        else:
            raise ValueError('unsupported wire type')
        if number in names:
            name = names[number]
            if wire != 0 or name in values:
                raise ValueError('ambiguous known field')
            if number > 1 and value > 0xffffffff:
                raise ValueError('uint32 overflow')
            values[name] = value
        else:
            unknown.append(payload[start:pos].hex())
    missing = [name for name in names.values() if name not in values]
    return dict(event_id=event_id, payload_hex=payload.hex(), fields=values,
                missing_schema_fields=missing, unknown_fields_hex=unknown,
                stock_completion_marker_observed=(event_id == 767 and values.get('force_stop', 0) != 0),
                transfer_complete_verified=False, source_verified=False,
                measurement_freshness_verified=False)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('event_id', type=int)
    parser.add_argument('payload_hex')
    args = parser.parse_args()
    print(json.dumps(decode(args.event_id, bytes.fromhex(args.payload_hex)), indent=2))
