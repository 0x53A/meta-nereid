import copy
import csv
import io
import json
from pathlib import Path
import tempfile
import unittest
from recording_power import analyze, battery_report, number, read_json, suspend_report

IDENTITY = '12345678-1234-1234-1234-123456789abc'
OTHER = '12345678-1234-1234-1234-123456789abd'


class RecordingPower(unittest.TestCase):
    def setUp(self):
        self.controller = dict(version=1, boot_id=IDENTITY, session_id=OTHER)
        self.intent = dict(self.controller, supervisor=f'hoki-recording-suspend-{IDENTITY}.service',
                           boottime_seconds=100, alarm_seconds=10)
        self.result = dict(self.controller, result='returned', elapsed_seconds=10.2,
                           awake_seconds=0.2, suspended_estimate_seconds=10.0,
                           alarm_expired=True)

    def documents(self):
        return [(f'suspend-{IDENTITY}-intent.json', self.intent),
                      (f'suspend-{IDENTITY}-result.json', self.result)]

    def test_optional_measurement_boundaries_preserve_legacy_summary(self):
        legacy = suspend_report(self.controller, self.documents())
        self.result.update(start_boottime_seconds=101, end_boottime_seconds=111.2)
        self.assertEqual(suspend_report(self.controller, self.documents()), legacy)

    def test_invalid_measurement_boundaries_are_rejected(self):
        valid = dict(self.result, start_boottime_seconds=101, end_boottime_seconds=111.2)
        cases = [dict(valid, start_boottime_seconds=99),
                 dict(valid, end_boottime_seconds=100),
                 dict(valid, end_boottime_seconds=112),
                 dict(valid, result='failed'), dict(valid, result='skipped')]
        for field in ('start_boottime_seconds', 'end_boottime_seconds'):
            missing = dict(valid)
            del missing[field]
            cases.append(missing)
            for value in (True, None, '101', float('nan'), float('inf')):
                cases.append(dict(valid, **{field: value}))
        for case in cases:
            with self.subTest(case=case), self.assertRaises(ValueError):
                self.result = case
                suspend_report(self.controller, self.documents())

    def test_saved_metadata_rejects_duplicate_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'metadata.json'
            for document, key, replacement in (
                (self.controller, 'boot_id', OTHER),
                (self.intent, 'boottime_seconds', 200),
                (self.result, 'elapsed_seconds', 20.2),
            ):
                with self.subTest(key=key):
                    encoded = json.dumps(document)
                    path.write_text(encoded[:-1] + ', ' + json.dumps(key)
                                    + ': ' + json.dumps(replacement) + '}')
                    with self.assertRaisesRegex(ValueError, 'duplicate metadata field'):
                        read_json(path)
                    path.write_text(encoded)
                    self.assertEqual(read_json(path), document)

    def test_boolean_schema_versions_are_not_integer_version_one(self):
        with self.subTest(document='controller'):
            controller = dict(self.controller, version=True)
            with self.assertRaises(ValueError):
                suspend_report(controller, self.documents())
        for index in (0, 1):
            with self.subTest(document=index):
                documents = copy.deepcopy(self.documents())
                documents[index][1]['version'] = True
                with self.assertRaises(ValueError):
                    suspend_report(self.controller, documents)

    def test_timing_range_errors_are_rejected_before_returning_a_report(self):
        with self.subTest(value='unrepresentable integer'):
            with self.assertRaises(ValueError):
                number(10 ** 400)
        self.result.update(elapsed_seconds=1e308, awake_seconds=0,
                           suspended_estimate_seconds=1e308)
        documents = self.documents()
        documents += [(f'suspend-{OTHER}-intent.json', copy.deepcopy(self.intent)),
                      (f'suspend-{OTHER}-result.json', copy.deepcopy(self.result))]
        with self.subTest(value='overflowing aggregate'):
            with self.assertRaises(ValueError):
                suspend_report(self.controller, documents)

    def test_returned_is_not_complete_session_or_proof_of_alarm_wake(self):
        report = suspend_report(self.controller, self.documents())
        self.assertEqual(report['returned_count'], 1)
        self.assertEqual(report['measured_call_suspended_estimate_seconds'], 10)
        self.assertEqual(report['alarm_expired_count'], 1)
        self.assertIsNone(report['whole_session_suspend_fraction'])
        self.assertFalse(report['alarm_was_wake_cause_verified'])
        self.assertFalse(report['complete_session_verified'])

    def test_missing_result_retains_uncertainty_and_orphan_refused(self):
        report = suspend_report(self.controller, self.documents()[:1])
        self.assertEqual(report['intents_without_result'], [IDENTITY])
        self.assertEqual(report['returned_count'], 0)
        with self.assertRaises(ValueError):
            suspend_report(self.controller, self.documents()[1:])
        empty = suspend_report(self.controller, [])
        self.assertFalse(empty['complete_session_verified'])

    def test_explicit_durability_deferral_is_not_measured_sleep(self):
        self.result = dict(self.controller, result='skipped',
                           reason='recording_pending_durability', suspend_requested=False)
        report = suspend_report(self.controller, self.documents())
        self.assertEqual(report['skipped_before_suspend_count'], 1)
        self.assertEqual(report['returned_count'], 0)
        self.assertEqual(report['measured_call_suspended_estimate_seconds'], 0)
        self.assertEqual(report['intents_without_result'], [])
        self.result['suspend_requested'] = True
        with self.assertRaises(ValueError): suspend_report(self.controller, self.documents())
        self.result['suspend_requested'] = False
        self.result['elapsed_seconds'] = 10
        with self.assertRaises(ValueError): suspend_report(self.controller, self.documents())

    def test_failed_mem_write_is_known_failure_not_measured_sleep(self):
        for errno in (1, 16, None):
            self.result = dict(self.controller, result='failed', stage='mem_write',
                               errno=errno, suspend_requested=True)
            report = suspend_report(self.controller, self.documents())
            self.assertEqual(report['failed_mem_write_count'], 1)
            self.assertEqual(report['failed_mem_writes'][0]['errno'], errno)
            self.assertEqual(report['returned_count'], 0)
            self.assertEqual(report['intents_without_result'], [])
            self.assertEqual(report['measured_call_suspended_estimate_seconds'], 0)
            self.assertIsNone(report['whole_session_suspend_fraction'])
        for key, value in [('errno', True), ('errno', 0), ('errno', -1),
                           ('errno', 4096), ('errno', '16'), ('stage', 'driver'),
                           ('suspend_requested', False), ('alarm_expired', False),
                           ('elapsed_seconds', 0)]:
            docs = copy.deepcopy(self.documents())
            docs[1][1][key] = value
            with self.assertRaises(ValueError):
                suspend_report(self.controller, docs)
        del self.result['errno']
        with self.assertRaises(ValueError):
            suspend_report(self.controller, self.documents())

    def test_wakeup_count_failure_never_claims_mem_or_sleep(self):
        self.result = dict(self.controller, result='failed', stage='wakeup_count_commit',
                           errno=22, retryable=True, suspend_requested=False)
        report = suspend_report(self.controller, self.documents())
        self.assertEqual(report['failed_wakeup_count_commit_count'], 1)
        self.assertEqual(report['failed_mem_write_count'], 0)
        self.assertEqual(report['returned_count'], 0)
        self.assertEqual(report['intents_without_result'], [])
        for key, value in [('suspend_requested', True), ('retryable', False),
                           ('retryable', 1), ('errno', 16), ('errno', None)]:
            docs = copy.deepcopy(self.documents())
            docs[1][1][key] = value
            with self.assertRaises(ValueError): suspend_report(self.controller, docs)

    def test_bad_identity_clock_and_duplicates_refused(self):
        for key, value in [('boot_id', OTHER), ('session_id', IDENTITY),
                           ('elapsed_seconds', float('nan')), ('awake_seconds', -1),
                           ('suspended_estimate_seconds', 20), ('alarm_expired', 1)]:
            docs = copy.deepcopy(self.documents())
            docs[1][1][key] = value
            with self.assertRaises(ValueError):
                suspend_report(self.controller, docs)
        with self.assertRaises(ValueError):
            suspend_report(self.controller, self.documents() + self.documents())

    def test_charger_boundary_counter_jump_does_not_become_energy(self):
        # Sequential sysfs reads can show charging alongside stale negative
        # current, while the gauge adjusts its charge-counter estimate.
        rows = [
            dict(boottime='100', capacity='3', status='Discharging',
                 charge_counter='2500', current_now='-30000'),
            dict(boottime='160', capacity='3', status='Charging',
                 charge_counter='18900', current_now='-23000'),
            dict(boottime='220', capacity='5', status='Charging',
                 charge_counter='23500', current_now='280000'),
        ]
        report = battery_report(rows)
        self.assertEqual(report['samples'], 3)
        self.assertEqual(report['observed_seconds'], 120)
        self.assertEqual(report['net_capacity_drop_percentage_points'], -2)
        self.assertEqual(report['sampled_statuses'], ['Charging', 'Discharging'])
        self.assertFalse(report['all_samples_discharging'])
        self.assertFalse(report['continuous_discharge_verified'])
        self.assertIsNone(report['integrated_energy_wh'])
        self.assertIsNone(report['estimated_battery_life_hours'])

    def test_discharging_status_does_not_certify_capacity_continuity(self):
        rows = [dict(boottime=str(time), capacity=str(capacity), status='Discharging')
                for time, capacity in [(100, 20), (160, 23), (220, 19)]]
        report = battery_report(rows)
        self.assertTrue(report['all_samples_discharging'])
        self.assertEqual(report['net_capacity_drop_percentage_points'], 1)
        self.assertFalse(report['continuous_discharge_verified'])
        self.assertIsNone(report['integrated_energy_wh'])
        self.assertIsNone(report['estimated_battery_life_hours'])

    def test_battery_sparse_samples_do_not_establish_energy_or_lifetime(self):
        rows = [dict(boottime='100', capacity='97', status='Discharging', display_blank=''),
                dict(boottime='160', capacity='96', status='Discharging', display_blank='')]
        report = battery_report(rows)
        self.assertEqual(report['observed_seconds'], 60)
        self.assertEqual(report['net_capacity_drop_percentage_points'], 1)
        self.assertEqual(report['sampled_display_blank_values'], ['unknown'])
        self.assertTrue(report['all_samples_discharging'])
        self.assertFalse(report['continuous_discharge_verified'])
        self.assertIsNone(report['integrated_energy_wh'])
        self.assertIsNone(report['estimated_battery_life_hours'])
        rows[1]['status'] = 'Charging'
        self.assertFalse(battery_report(rows)['all_samples_discharging'])
        rows[1]['boottime'] = '99'
        with self.assertRaises(ValueError):
            battery_report(rows)
        self.assertIsNone(battery_report([])['observed_seconds'])

    def test_incomplete_csv_rows_are_rejected_with_row_and_field(self):
        header = 'boottime,capacity,status,display_blank\n'
        first = '100,97,Discharging,\n'
        for tail, field in [('160,96', 'status'), ('160', 'capacity'),
                            ('160,96,,', 'status'), ('160,96,Discharging,,extra', 'columns')]:
            with self.subTest(tail=tail):
                rows = csv.DictReader(io.StringIO(header + first + tail))
                with self.assertRaisesRegex(ValueError, f'battery sample 2.*{field}'):
                    battery_report(rows)
        # Omitted optional display column is still a usable sample.
        rows = csv.DictReader(io.StringIO(header + first + '160,96,Discharging'))
        self.assertEqual(battery_report(rows)['sampled_display_blank_values'], ['unknown'])

    def test_battery_numeric_errors_do_not_turn_into_samples(self):
        for field in ('boottime', 'capacity'):
            for value in (None, True, 'NaN', 'Infinity', '', 'bad'):
                with self.subTest(field=field, value=value):
                    row = dict(boottime='100', capacity='97', status='Discharging')
                    row[field] = value
                    with self.assertRaisesRegex(ValueError, f'battery sample 1.*{field}'):
                        battery_report([row])

    def test_csv_schema_and_quoting_are_validated_before_reporting(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            hal = root / 'recording/hal'
            hal.mkdir(parents=True)
            (hal / 'controller.json').write_text(json.dumps(self.controller))
            self.assertIsNone(analyze(root)['battery'])
            telemetry = root / 'telemetry.csv'
            for contents in (
                '', 'boottime,capacity\n',
                'boottime,capacity,capacity,status\n100,97,2,Discharging\n',
                'boottime,capacity,status,\n100,97,Discharging,\n',
                'boottime,capacity,status\n100,97,"Discharging',
            ):
                with self.subTest(contents=contents):
                    telemetry.write_text(contents)
                    with self.assertRaisesRegex(ValueError, 'battery CSV'):
                        analyze(root)
            telemetry.write_text('boottime,capacity,status\n')
            self.assertEqual(analyze(root)['battery']['samples'], 0)
            # Additional named telemetry columns remain valid, as do quoted
            # values and a complete final row with no trailing newline.
            telemetry.write_text('boottime,capacity,status,current_now\n'
                                 '100,97,"Discharging",-30000\n'
                                 '160,96,"Discharging",-29000')
            report = analyze(root)['battery']
            self.assertEqual(report['samples'], 2)
            self.assertEqual(report['net_capacity_drop_percentage_points'], 1)


if __name__ == '__main__':
    unittest.main()
