import copy
import unittest
from hal_coverage import coverage


class Coverage(unittest.TestCase):
    def setUp(self):
        self.controller = dict(version=1, session_id='session', boot_id='boot', phase='closed',
                               activated_handles=[1, 2], selected=[
            dict(sensor=dict(handle=1, type=1, flags=1, name='accel'), period_ns=40, latency_ns=200),
            dict(sensor=dict(handle=2, type=21, flags=3, name='HR'), period_ns=200, latency_ns=200)])
        self.archive = dict(checkpoint_claim_and_bytes_consistent=True, channels=[
            dict(handle=1, type=1, records=3), dict(handle=2, type=0, records=1),
            dict(handle=9, type=31, records=2)])

    def test_silent_activation_metadata_and_unexpected_output(self):
        report = coverage(self.controller, self.archive)
        self.assertEqual((report['selected_count'], report['activated_count'],
                          report['selected_with_output'], report['selected_without_output']), (2, 2, 1, 1))
        self.assertEqual(report['metadata_records'], 1)
        self.assertEqual(report['unexpected_channels'], [dict(handle=9, type=31, records=2)])
        hr = report['channels'][1]
        self.assertEqual((hr['reporting_mode'], hr['records'], hr['activation_recorded']), ('on_change', 0, True))
        self.assertFalse(hr['freshness_verified'])
        self.assertFalse(report['controller_completion_verified'])

    def test_ambiguous_selection_and_activation_rejected(self):
        for key, value in [('activated_handles', [1, 1]), ('activated_handles', [8]),
                           ('selected', self.controller['selected'] * 2)]:
            bad = copy.deepcopy(self.controller)
            bad[key] = value
            with self.assertRaises(ValueError):
                coverage(bad, self.archive)

    def test_output_without_activation_does_not_establish_success_or_freshness(self):
        # Startup may have emitted buffered output before activation metadata
        # was recorded. A metadata record with another selected handle must
        # not count as that sensor's measurement either.
        self.controller['activated_handles'] = []
        self.controller['phase'] = 'failed'
        report = coverage(self.controller, self.archive)
        self.assertEqual(report['activated_count'], 0)
        self.assertEqual(report['selected_with_output'], 1)
        self.assertEqual(report['selected_without_output'], 1)
        observed, silent = report['channels']
        self.assertTrue(observed['output_observed'])
        self.assertFalse(silent['output_observed'])
        self.assertEqual((observed['records'], silent['records']), (3, 0))
        self.assertEqual(report['metadata_records'], 1)
        for row in report['channels']:
            self.assertFalse(row['activation_recorded'])
            self.assertFalse(row['freshness_verified'])
            self.assertFalse(row['continuity_verified'])
            self.assertIsNone(row['missing_sample_count'])
        self.assertEqual(report['controller_phase'], 'failed')
        self.assertFalse(report['controller_completion_verified'])
        self.assertFalse(report['metadata_to_events_provenance_verified'])

    def test_schema_and_requested_timing_require_nonnegative_integers(self):
        for version in (True, 1.0, '1'):
            with self.subTest(version=version):
                bad = copy.deepcopy(self.controller)
                bad['version'] = version
                with self.assertRaises(ValueError):
                    coverage(bad, self.archive)
        for field in ('period_ns', 'latency_ns'):
            for value in (True, -1, 1.5, '200', None):
                with self.subTest(field=field, value=value):
                    bad = copy.deepcopy(self.controller)
                    bad['selected'][0][field] = value
                    with self.assertRaises(ValueError):
                        coverage(bad, self.archive)

    def test_data_and_metadata_record_counts_require_nonnegative_integers(self):
        for index in (0, 1, 2):
            for count in (True, -1, 1.5, '3', None):
                with self.subTest(channel=index, count=count):
                    bad = copy.deepcopy(self.archive)
                    bad['channels'][index]['records'] = count
                    with self.assertRaises(ValueError):
                        coverage(self.controller, bad)

    def test_window_stats_preserve_archive_coverage_counts(self):
        self.archive['source_window_ns'] = [100, 200]
        stats = self.archive['channels'][0]
        stats.update(source_before_window=3, source_after_window=0,
                     source_window_statistics={'records': 0, 'max_interval_ns': None})
        report = coverage(self.controller, self.archive)
        self.assertEqual(report['source_window_ns'], [100, 200])
        self.assertEqual(report['selected_with_output'], 1)
        row = report['channels'][0]
        self.assertEqual(row['records'], 3)
        self.assertEqual(row['timestamp_statistics']['source_window_statistics']['records'], 0)
        self.assertFalse(row['freshness_verified'])

    def test_requested_period_comparison_is_only_continuous_and_not_loss_count(self):
        self.archive['channels'][0]['max_interval_ns'] = 200
        self.archive['channels'].append(dict(handle=2, type=21, records=2, max_interval_ns=1000))
        accel, hr = coverage(self.controller, self.archive)['channels']
        self.assertEqual(accel['maximum_interval_requested_periods'], 5)
        self.assertIsNone(hr['maximum_interval_requested_periods'])
        self.assertIsNone(accel['missing_sample_count'])
        self.assertFalse(accel['continuity_verified'])
        self.controller['selected'][0]['period_ns'] = 0
        self.assertIsNone(coverage(self.controller, self.archive)['channels'][0]['maximum_interval_requested_periods'])
        self.controller['selected'][0]['period_ns'] = -1
        with self.assertRaises(ValueError):
            coverage(self.controller, self.archive)


if __name__ == '__main__':
    unittest.main()
