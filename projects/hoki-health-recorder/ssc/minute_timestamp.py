"""Decode stock format-0080 E2 records without correcting their timeline.

Grounded in LegacyFileParser::parse_event (0x1e2e80) and the stock
fitness_data.proto InfoCode/Timestamp descriptors; see task0283. Raw bytes remain
authoritative. A decoded timestamp is not evidence of fresh health measurements.
"""
import struct

INFO_CODES = {0: 'NONE', 1: 'SYSTEM_RESET', 2: 'HARDWARE_RESET',
              3: 'TIME_CHANGED', 5: 'HARDWARE_BOOTUP'}


def decode(raw):
    if len(raw) != 10 or raw[0] != 0xe2:
        raise ValueError('expected one ten-byte E2 event')
    subtype = raw[1]
    seconds, milliseconds, zone = struct.unpack_from('<IHh', raw, 2)
    # Stock subtype4 writes only Timestamp, not Entry.info_code. Subtype2
    # writes info_code but returns before setting Timestamp or minute phase.
    has_timestamp = subtype != 2
    return {
        'raw_hex': raw.hex(), 'subtype': subtype,
        'info_code': None if subtype == 4 else subtype,
        'info_name': INFO_CODES.get(subtype),
        'timestamp_only': subtype == 4,
        'seconds_word': seconds, 'milliseconds_word': milliseconds,
        'timezone_word': zone,
        'stock_timestamp_ms': seconds * 1000 + milliseconds if has_timestamp else None,
        'stock_minute_second': seconds % 60 if has_timestamp else None,
        'fraction_in_standard_range': milliseconds < 1000,
        'timeline_corrected': False,
        'fresh_measurement_verified': False,
    }
