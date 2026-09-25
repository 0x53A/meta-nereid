#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec=importlib.util.spec_from_file_location('summarize',Path(__file__).resolve().parents[1]/'summarize.py')
module=importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class SummaryTests(unittest.TestCase):
    def run_log(self, data):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'test.jsonl';path.write_bytes(data)
            return module.summarize(path)

    def row(self, sequence, event, elapsed=0, **extra):
        return (json.dumps(dict(sequence=sequence,event=event,elapsed_ms=elapsed,boottime_ms=1000+elapsed,**extra))+'\n').encode()

    def test_torn_utf8(self):
        summary=self.run_log(self.row(1,'session_start')+b'{"message":"\xc3')
        self.assertTrue(summary['truncated_final_line'])
        self.assertFalse(summary['complete'])
        self.assertEqual(summary['events'],1)

    def test_invalid_numeric_metadata_is_not_a_measurement(self):
        base = dict(sequence=1, event='session_start', boottime_ms=1000, elapsed_ms=0)
        for field in ('sequence', 'boottime_ms', 'elapsed_ms'):
            for value in (True, '1', 1.5, -1, None, float('nan'), float('inf')):
                with self.subTest(field=field, value=value):
                    row = dict(base, **{field: value})
                    with self.assertRaises(ValueError):
                        self.run_log((json.dumps(row)+'\n').encode())
        with self.assertRaises(ValueError):
            self.run_log(self.row(0, 'session_start'))
        for value in ([], None, 1):
            with self.subTest(document=value), self.assertRaises(ValueError):
                self.run_log((json.dumps(value)+'\n').encode())

    def test_regressing_clocks_are_rejected_but_equal_times_are_valid(self):
        first = self.row(1, 'session_start', 100)
        for clock in ('boottime_ms', 'elapsed_ms'):
            row = dict(sequence=2, event='heartbeat', boottime_ms=1100, elapsed_ms=100)
            row[clock] -= 1
            with self.subTest(clock=clock), self.assertRaises(ValueError):
                self.run_log(first+(json.dumps(row)+'\n').encode())
        self.assertEqual(self.run_log(first+self.row(2, 'heartbeat', 100))['events'], 2)

    def test_freshness_requires_a_boolean(self):
        for value in ('false', 1, [], None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.run_log(self.row(1, 'dbus', fresh_for_session=value))

    def test_corrupt_middle_is_not_silently_ignored(self):
        with self.assertRaises(UnicodeDecodeError):
            self.run_log(self.row(1,'session_start')+b'{"message":"\xc3\n'+self.row(2,'heartbeat',5000))

    def test_quiet_tail_and_no_satellites(self):
        summary=self.run_log(self.row(1,'session_start')+self.row(2,'session_end',20000,reason='user_stop'))
        self.assertEqual(summary['max_satellite_gap_ms'],20000)
        data=self.row(1,'session_start')+self.row(2,'dbus',1000,member='SatelliteChanged',arguments=[0,0,0,[],[]])+self.row(3,'session_end',31000,reason='user_stop')
        summary=self.run_log(data)
        self.assertEqual(summary['max_satellite_gap_ms'],30000)
        self.assertTrue(summary['complete'])

    def test_live_satellite_statistics_exclude_initial_snapshots(self):
        data = self.row(1, 'session_start')
        data += self.row(2, 'dbus', 100, source='snapshot', member='GetSatellite',
                         arguments=[0, 20, 30, [], [[1, 0, 0, 99]]])
        data += self.row(3, 'dbus', 5000, source='signal', member='SatelliteChanged',
                         arguments=[0, 2, 3, [1, 2], [[1, 0, 0, 20], [2, 0, 0, 0], [3, 0, 0, 35]]])
        data += self.row(4, 'dbus', 13000, source='signal', member='SatelliteChanged',
                         arguments=[0, 0, 0, [], []])
        data += self.row(5, 'session_end', 16000, reason='user_stop')
        summary = self.run_log(data)
        self.assertEqual(summary['max_used'], 2)
        self.assertEqual(summary['max_listed'], 3)
        self.assertEqual(summary['max_snr'], 35)
        self.assertEqual(summary['max_satellite_gap_ms'], 8000)
        self.assertEqual(summary['last_satellite_age_ms'], 3000)
        self.assertEqual(summary['event_counts']['dbus:GetSatellite'], 1)
        self.assertEqual(summary['event_counts']['dbus:SatelliteChanged'], 2)

    def test_malformed_satellite_statistics_are_rejected(self):
        cases = [None, [], [0, 1], [0, True, 1, [], []],
                 [0, 0, -1, [], []], [0, 0, '1', [], []],
                 [0, 0, 1, [], None], [0, 0, 1, [], [[1, 2, 3]]],
                 [0, 0, 1, [], [[1, 2, 3, True]]],
                 [0, 0, 1, [], [[1, 2, 3, float('inf')]]],
                 [0, 0, 1, [], [[1, 2, 3, float('nan')]]]]
        for args in cases:
            with self.subTest(arguments=args), self.assertRaisesRegex(ValueError, 'satellite'):
                self.run_log(self.row(1, 'dbus', member='SatelliteChanged', arguments=args))

    def test_satellite_counts_are_observations_not_recomputed_from_lists(self):
        summary = self.run_log(self.row(1, 'dbus', member='SatelliteChanged',
                                       arguments=[0, 2, 3, [], [[1, 0, 0, -1]]]))
        self.assertEqual(summary['max_used'], 2)
        self.assertEqual(summary['max_listed'], 3)
        self.assertEqual(summary['max_snr'], 0)

    def test_quiet_interval_before_first_report_is_counted(self):
        data = self.row(1, 'session_start')
        data += self.row(2, 'dbus', 18000, member='SatelliteChanged',
                         arguments=[0, 0, 0, [], []])
        data += self.row(3, 'session_end', 19000, reason='user_stop')
        summary = self.run_log(data)
        self.assertEqual(summary['max_satellite_gap_ms'], 18000)
        self.assertEqual(summary['last_satellite_age_ms'], 1000)

    def test_freshness_uses_recorded_metadata_and_preserves_raw_counts(self):
        data = self.row(1, 'session_start')
        for sequence, source, fresh in [(2, 'snapshot', False), (3, 'signal', False),
                                        (4, 'signal', True), (5, 'signal', True)]:
            data += self.row(sequence, 'dbus', sequence * 1000,
                             source=source, member='GetPosition' if source == 'snapshot' else 'PositionChanged',
                             fresh_for_session=fresh,
                             arguments=[3, 0, 51.23456789, 7.98765432, 0, [3, 5, 5]])
        summary = self.run_log(data)
        self.assertEqual(summary['fresh_position_signals'], 2)
        self.assertEqual(summary['event_counts']['dbus:PositionChanged'], 3)
        self.assertEqual(summary['events'], 5)
        self.assertFalse(summary['complete'])
        # Summaries must not copy raw coordinates into their output.
        self.assertNotIn('51.23456789', json.dumps(summary))
        self.assertNotIn('7.98765432', json.dumps(summary))

    def test_end_marker_and_sequence_integrity_are_reported_separately(self):
        data = self.row(1, 'session_start') + self.row(3, 'session_end', 1000, reason='size_limit')
        summary = self.run_log(data)
        self.assertTrue(summary['complete'])
        self.assertTrue(summary['sequence_discontinuity'])
        self.assertEqual(summary['end_reason'], 'size_limit')
        # An end marker cannot hide a later interrupted write.
        summary = self.run_log(data + b'{"sequence":4')
        self.assertFalse(summary['complete'])
        self.assertTrue(summary['truncated_final_line'])

    def test_complete_json_without_newline_is_not_a_torn_record(self):
        data = self.row(1, 'session_start') + self.row(2, 'session_end', 1000, reason='user_stop').rstrip(b'\n')
        summary = self.run_log(data)
        self.assertTrue(summary['complete'])
        self.assertNotIn('truncated_final_line', summary)
        self.assertNotIn('sequence_discontinuity', summary)

    def test_newline_terminated_corrupt_json_is_not_tolerated_as_torn_tail(self):
        with self.assertRaises(json.JSONDecodeError):
            self.run_log(self.row(1, 'session_start') + b'{unfinished\n')

if __name__=='__main__':unittest.main()
