import hashlib
import json
from pathlib import Path
import struct
import tempfile
import tracemalloc
import unittest

from verify_hal import HEADER, verify


def record(arrival, timestamp, handle=1, typ=1):
    return struct.pack('<qqII', arrival, timestamp, handle, typ) + bytes(64)


class SavedCapture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def checkpoint(self, count, segment=0, segment_bytes=None, total_bytes=None, **extra):
        self.cp = dict(version=1, segment=segment,
                       segment_bytes=segment_bytes if segment_bytes is not None else 16+88*count,
                       total_bytes=total_bytes if total_bytes is not None else 16+88*count,
                       records=count, complete=True, final=True, dropped=0, input_failures=0,
                       sequence_missing=0, **extra)
        (self.root/'checkpoint.json').write_text(json.dumps(self.cp))

    def test_segment_boundaries_hash_and_timestamp_reset(self):
        a=HEADER+record(25,10)+record(35,30)
        b=HEADER+record(45,20)
        (self.root/'events-000000.bin').write_bytes(a)
        (self.root/'events-000001.bin').write_bytes(b)
        self.checkpoint(3,segment=1,segment_bytes=len(b),total_bytes=len(a)+len(b))
        result=verify(self.root)
        self.assertTrue(result['checkpoint_claim_and_bytes_consistent'])
        self.assertFalse(result['controller_completion_verified'])
        self.assertFalse(result['sensor_freshness_verified'])
        self.assertEqual(result['files'][0]['sha256'],hashlib.sha256(a).hexdigest())
        row=result['channels'][0]
        self.assertEqual((row['records'],row['nonincreasing'],row['mean_interval_ns'],row['max_age_ns']),(3,1,5,25))

    def test_unacknowledged_tail_and_extra_segment_are_not_final(self):
        data=HEADER+record(25,10)+b'partial'
        (self.root/'events-000000.bin').write_bytes(data)
        (self.root/'events-000001.bin').write_bytes(HEADER)
        self.checkpoint(1)
        result=verify(self.root)
        self.assertFalse(result['checkpoint_claim_and_bytes_consistent'])
        self.assertEqual(result['durable_records'],1)
        self.assertEqual(result['files'][0]['unacknowledged_tail_bytes'],7)
        self.assertEqual(result['files'][0]['sha256'],hashlib.sha256(data).hexdigest())
        self.assertEqual(result['uncheckpointed_files'],[{'name':'events-000001.bin','bytes':16}])

    def test_invalid_header_length_and_totals_fail(self):
        path=self.root/'events-000000.bin'
        for data,count in [(bytes(104),1),(HEADER+b'partial',1),(HEADER+record(25,10),2)]:
            path.write_bytes(data);self.checkpoint(count)
            with self.assertRaises(ValueError):verify(self.root)
        path.write_bytes(HEADER+record(25,10));self.checkpoint(1)
        self.cp['records']=True
        (self.root/'checkpoint.json').write_text(json.dumps(self.cp))
        with self.assertRaises(ValueError):verify(self.root)

    def test_duplicate_checkpoint_counter_cannot_hide_recording_loss(self):
        (self.root / 'events-000000.bin').write_bytes(HEADER + record(25, 10))
        self.checkpoint(1)
        self.cp['dropped'] = 1
        text = json.dumps(self.cp)[:-1] + ', "dropped": 0}'
        # Default JSON decoding would silently retain zero and allow a clean claim.
        self.assertEqual(json.loads(text)['dropped'], 0)
        (self.root / 'checkpoint.json').write_text(text)
        with self.assertRaisesRegex(ValueError, 'duplicate metadata field'):
            verify(self.root)

    def test_each_incomplete_condition_independently_prevents_clean_claim(self):
        path = self.root / 'events-000000.bin'
        extra = self.root / 'events-000001.bin'
        durable = HEADER + record(25, 10)
        cases = [('complete', False), ('final', False), ('dropped', 1),
                 ('input_failures', 1), ('sequence_missing', 1),
                 ('partial_tail', b'partial'),
                 ('whole_record_tail', record(35, 20)), ('extra_segment', HEADER)]
        for condition, value in cases:
            with self.subTest(condition=condition):
                path.write_bytes(durable)
                extra.unlink(missing_ok=True)
                self.checkpoint(1)
                if condition.endswith('_tail'):
                    path.write_bytes(durable + value)
                elif condition == 'extra_segment':
                    extra.write_bytes(value)
                else:
                    self.cp[condition] = value
                    (self.root / 'checkpoint.json').write_text(json.dumps(self.cp))
                result = verify(self.root)
                self.assertFalse(result['checkpoint_claim_and_bytes_consistent'])
                self.assertEqual(result['durable_records'], 1)
                self.assertEqual(result['durable_bytes'], len(durable))
                self.assertEqual(result['channels'][0]['records'], 1)
                self.assertFalse(result['controller_completion_verified'])
                self.assertFalse(result['sensor_freshness_verified'])
                self.assertEqual(result['files'][0]['unacknowledged_tail_bytes'],
                                 len(value) if condition.endswith('_tail') else 0)
                self.assertEqual(bool(result['uncheckpointed_files']),
                                 condition == 'extra_segment')

    def test_large_capture_does_not_retain_each_event(self):
        count=100000
        with (self.root/'events-000000.bin').open('wb') as stream:
            stream.write(HEADER)
            block=b''.join(record(25+i,10+i) for i in range(1000))
            for _ in range(count//1000):stream.write(block)
        self.checkpoint(count)
        tracemalloc.start()
        try:
            result=verify(self.root, (10, 1009))
            _,peak=tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertEqual(result['durable_records'],count)
        self.assertLess(peak,4*1024*1024)

    def test_largest_intervals_are_bounded_and_cross_segments(self):
        records = []
        timestamp = 0
        for interval in range(21):
            timestamp += interval
            records.append(record(timestamp + 1000, timestamp))
            records.append(record(timestamp + 1000, 0, handle=0, typ=0))
        first = HEADER + b''.join(records[:30])
        second = HEADER + b''.join(records[30:])
        (self.root/'events-000000.bin').write_bytes(first)
        (self.root/'events-000001.bin').write_bytes(second)
        self.checkpoint(42, segment=1, segment_bytes=len(second),
                        total_bytes=len(first)+len(second))
        result = verify(self.root)
        metadata, sensor = result['channels']
        self.assertNotIn('largest_source_intervals', metadata)
        examples = sensor['largest_source_intervals']
        self.assertEqual([e['interval_ns'] for e in examples], list(range(20, 12, -1)))
        boundary = next(e for e in examples if e['interval_ns'] == 15)
        self.assertEqual((boundary['previous_record_index'], boundary['record_index']), (28, 30))
        self.assertEqual(boundary['arrival_ns'] - boundary['previous_arrival_ns'], 15)
        self.assertEqual(boundary['timestamp_ns'] - boundary['previous_timestamp_ns'], 15)

    def test_source_window_keeps_cached_records_and_integrity_checks(self):
        # The window is inclusive and follows source time, not delivery time.
        first = HEADER + record(1000, 1) + record(1000, 100)
        second = (HEADER + record(1000, 120) + record(1000, 130)
                  + record(1000, 150) + record(1000, 1, handle=2)
                  + record(1000, 0, handle=0, typ=0))
        (self.root/'events-000000.bin').write_bytes(first)
        (self.root/'events-000001.bin').write_bytes(second)
        self.checkpoint(7, segment=1, segment_bytes=len(second),
                        total_bytes=len(first)+len(second))
        result = verify(self.root, (100, 130))
        metadata, sensor, cached_only = result['channels']
        window = sensor['source_window_statistics']
        self.assertTrue(result['checkpoint_claim_and_bytes_consistent'])
        self.assertEqual(result['durable_records'], 7)
        self.assertEqual(sensor['records'], 5)
        self.assertEqual(sensor['max_interval_ns'], 99)
        self.assertEqual((sensor['source_before_window'], sensor['source_after_window']), (1, 1))
        self.assertEqual((window['records'], window['max_interval_ns'], window['max_age_ns']), (3, 20, 900))
        self.assertEqual(window['largest_source_intervals'][0]['previous_record_index'], 1)
        self.assertEqual(window['largest_source_intervals'][0]['record_index'], 2)
        self.assertEqual(cached_only['source_window_statistics']['records'], 0)
        self.assertNotIn('max_age_ns', cached_only['source_window_statistics'])
        self.assertNotIn('source_window_statistics', metadata)
        self.cp['records'] = 3  # A window count must never replace the archive total.
        (self.root/'checkpoint.json').write_text(json.dumps(self.cp))
        with self.assertRaises(ValueError):
            verify(self.root, (100, 130))

    def test_invalid_source_windows(self):
        for window in ((1, 0), (-1, 1), (True, 2), (0, 1.0), (1,), (1, 2, 3), 5, '12'):
            with self.subTest(window=window), self.assertRaises(ValueError):
                verify(self.root, window)


if __name__=='__main__':
    unittest.main()
