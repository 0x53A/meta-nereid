import json
import math
from pathlib import Path
import struct
import tempfile
import unittest

from health_decode import decode_health, summarize, SPO2, BEAT_INTERVAL
from verify_hal import HEADER


def payload(value=95, confidence=90, algorithm=5, signal=0):
    return struct.pack('<4f', value, confidence, algorithm, signal) + bytes(48)


class HealthDecodeTests(unittest.TestCase):
    def test_progress_is_never_a_final_measurement(self):
        for state in range(5):
            row = decode_health(SPO2, payload(algorithm=state))
            self.assertFalse(row['stock_ui_eligible_default80'])
            self.assertIn('not_final', row['reasons'])
        self.assertEqual(decode_health(SPO2, payload(algorithm=4))['phase'], 'provisional')

    def test_stock_thresholds_and_truncation(self):
        for value, confidence, service, ui in [(95,90,True,True), (95,79.9,False,False),
                                              (80.9,80,True,False), (81,80,True,True),
                                              (79.9,100,False,False)]:
            row = decode_health(SPO2, payload(value, confidence))
            self.assertEqual(row['stock_service_eligible_default80'], service)
            self.assertEqual(row['stock_ui_eligible_default80'], ui)

    def test_movement_final_and_zero_progress_are_distinct(self):
        row = decode_health(SPO2, payload(0, 0, 5, 8))
        self.assertEqual(row['phase'], 'final')
        self.assertEqual(row['signal_label'], 'movement')
        self.assertFalse(row['stock_ui_eligible_default80'])
        self.assertEqual(decode_health(SPO2, payload(0,0,4,0))['phase'], 'provisional')

    def test_unknown_states_and_nonfinite_payload_preserved(self):
        for raw in [payload(algorithm=99), payload(signal=3), payload(algorithm=5.5),
                    payload(value=math.nan), payload(confidence=math.inf)]:
            row = decode_health(SPO2, raw)
            self.assertEqual(bytes.fromhex(row['raw_payload_hex']), raw)
            self.assertFalse(row['stock_service_eligible_default80'])
            json.dumps(row, allow_nan=False)
        self.assertEqual(decode_health(SPO2, payload(signal=3))['signal_label'], 'unknown')
        self.assertEqual(decode_health(SPO2, payload(signal=10))['signal_label'], 'fit_too_tight')

    def test_beat_interval_is_not_respiration(self):
        row = decode_health(BEAT_INTERVAL, payload(750))
        self.assertEqual((row['kind'], row['unit'], row['value']), ('beat_interval','ms',750))
        self.assertFalse(decode_health(BEAT_INTERVAL, payload(0))['valid_numeric'])
        self.assertIsNone(decode_health(1, bytes(64)))
        with self.assertRaises(ValueError):
            decode_health(SPO2, bytes(16))

    def test_archive_window_and_unacknowledged_tail(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            record = lambda stamp: struct.pack('<qqII64s', stamp+100, stamp, 1, SPO2, payload())
            data = HEADER + record(10) + record(20)
            (root/'events-000000.bin').write_bytes(data + record(30))
            (root/'checkpoint.json').write_text(json.dumps(dict(version=1,segment=0,
                segment_bytes=len(data),total_bytes=len(data),records=2,complete=True,final=True,
                dropped=0,input_failures=0,sequence_missing=0)))
            result = summarize(root, (20,30))
            self.assertEqual(result['counts']['spo2'],1)
            self.assertEqual(result['counts']['health_records_outside_window'],1)
            self.assertFalse(result['checkpoint_claim_and_bytes_consistent'])
            self.assertEqual(result['source_files'][0]['unacknowledged_tail_bytes'],88)
