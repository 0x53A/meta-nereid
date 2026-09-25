import sys
from pathlib import Path
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from heartbeat_payload import decode


class Payload(unittest.TestCase):
    def test_full_width_quality_and_unknown_fields_preserved(self):
        data = bytes.fromhex('0801100218ffffffff0f22027879')
        result = decode(1028, data)
        self.assertEqual(result['fields'], dict(timestamp=1, ppg=2, quality=0xffffffff))
        self.assertEqual(result['unknown_fields_hex'], ['22027879'])
        self.assertEqual(result['payload_hex'], data.hex())
        self.assertEqual(result['missing_schema_fields'], [])
        self.assertFalse(result['stock_completion_marker_observed'])

    def test_partial_control_does_not_claim_transfer_complete(self):
        result = decode(767, b'\x10\x01')
        self.assertTrue(result['stock_completion_marker_observed'])
        self.assertEqual(result['missing_schema_fields'], ['unix_ts_sec'])
        self.assertFalse(result['transfer_complete_verified'])
        self.assertFalse(decode(767, b'')['stock_completion_marker_observed'])
        self.assertFalse(decode(1029, b'\x08\x01\x10\x01\x18\x01')['stock_completion_marker_observed'])

    def test_unknown_wire_values_survive_without_becoming_measurements(self):
        fields = ['210001020304050607', '2d08090a0b', '32027879', '388001']
        data = bytes.fromhex(''.join(fields))
        result = decode(1028, data)
        self.assertEqual(result['unknown_fields_hex'], fields)
        self.assertEqual(result['payload_hex'], data.hex())
        self.assertEqual(result['fields'], {})
        self.assertEqual(result['missing_schema_fields'], ['timestamp', 'ppg', 'quality'])
        self.assertFalse(result['measurement_freshness_verified'])
        self.assertFalse(result['source_verified'])

    def test_payload_bound_and_truncated_unknown_fixed_fields(self):
        # Unknown field 4, length-delimited: 3-byte header + 4093-byte body.
        payload = bytes.fromhex('22fd1f') + bytes(4093)
        self.assertEqual(len(payload), 4096)
        self.assertEqual(decode(1029, payload)['unknown_fields_hex'], [payload.hex()])
        with self.assertRaisesRegex(ValueError, 'bound'):
            decode(1029, payload+b'\x00')
        for payload in (b'\x21'+bytes(7), b'\x25'+bytes(3)):
            with self.subTest(payload=payload), self.assertRaisesRegex(ValueError, 'truncated field'):
                decode(1028, payload)

    def test_malformed_or_ambiguous_input_rejected(self):
        for data in [b'\x08', b'\x08\x01\x08\x02', b'\x0a\x00',
                     b'\x08' + b'\xff'*9+b'\x02', b'\x18\x80\x80\x80\x80\x10',
                     b'\x00', b'\x22\x05x']:
            with self.assertRaises(ValueError):
                decode(1028, data)


if __name__ == '__main__':
    unittest.main()
