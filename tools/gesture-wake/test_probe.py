import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import analyze
import probe


class ProbeTests(unittest.TestCase):
    def test_raw_accel_and_gesture_have_separate_latencies_and_cleanup_selection(self):
        config = dict(mode='suspend', type=26, raw_accel=True, accel_batch_ms=20000)
        env = probe.configured_env(Path('/trial'), config)
        self.assertEqual(env['HOKI_SENSOR_TYPES'], '1,26')
        self.assertEqual(env['HOKI_BATCH_MS'], '0')
        self.assertEqual(env['HOKI_ACCEL_BATCH_MS'], '20000')
        config['mode'] = 'baseline'
        self.assertEqual(probe.configured_env(Path('/trial'), config)['HOKI_SENSOR_TYPES'], '1')
        config['accel_batch_ms'] = 0
        with self.assertRaises(ValueError):
            probe.configured_env(Path('/trial'), config)

    def test_raw_accel_requires_wakeup_continuous_descriptor(self):
        row = 'DESCRIPTOR handle=55 type=1 min_delay_us=20000 max_delay_us=1000000 fifo_reserved=2000 fifo_max=10000 flags=1\n'
        self.assertEqual(probe.candidate(row, 1)['handle'], 55)
        with self.assertRaises(RuntimeError):
            probe.candidate(row.replace('flags=1', 'flags=3'), 1)

    def test_missing_pm_test_requires_live_kernel_config_proof(self):
        probe.validate_pm_test('[none] core devices')
        probe.validate_pm_test(None, '# CONFIG_PM_DEBUG is not set\nCONFIG_SUSPEND=y\n')
        for setting, config in [('none [devices]', None), (None, None), (None, 'CONFIG_PM_DEBUG=y')]:
            with self.assertRaises(RuntimeError):
                probe.validate_pm_test(setting, config)

    def test_stopped_daemon_may_have_failed_but_must_have_no_process(self):
        probe.validate_stopped('inactive', '0')
        probe.validate_stopped('failed', '0')
        for state, pid in [('active', '0'), ('failed', '12'), ('activating', '0')]:
            with self.assertRaises(RuntimeError):
                probe.validate_stopped(state, pid)

    def test_status_uses_versioned_power_protocol(self):
        with patch.object(probe.socket, 'socket') as factory:
            client = factory.return_value.__enter__.return_value
            client.recv.return_value = b'{"ok":true}\n'
            self.assertEqual(probe.power_status(), {'ok': True})
            sent = json.loads(client.sendall.call_args.args[0])
            self.assertEqual(sent, {'version': 1, 'command': 'status'})

    def test_selection_requires_unique_wakeup_and_correct_mode(self):
        row = 'DESCRIPTOR handle=1 type=26 min_delay_us=0 max_delay_us=0 fifo_reserved=0 fifo_max=0 flags=7\n'
        self.assertEqual(probe.candidate(row, 26)['handle'], 1)
        for text in ('', row + row, row.replace('flags=7', 'flags=6'), row.replace('flags=7', 'flags=5')):
            with self.assertRaises(RuntimeError):
                probe.candidate(text, 26)
        self.assertEqual(probe.candidate(row.replace('type=26', 'type=17').replace('flags=7', 'flags=5'), 17)['flags'], 5)

    def test_power_requires_disabled_policy_and_no_suspend_inhibitors(self):
        status = dict(config=dict(enabled=False, sensor_profile='off'), sensor_fault=False, inhibitors=[])
        probe.validate_power(status, True)
        for field, value in [('enabled', True), ('sensor_profile', 'full')]:
            bad = copy.deepcopy(status)
            bad['config'][field] = value
            with self.assertRaises(RuntimeError):
                probe.validate_power(bad, False)
        for change in [dict(sensor_fault=True), dict(inhibitors=[dict(cpu=True)])]:
            with self.assertRaises(RuntimeError):
                probe.validate_power(dict(status, **change), True)
        with self.assertRaises(RuntimeError):
            probe.validate_power({}, True)

    def test_environment_does_not_inherit_other_capture_selection(self):
        with patch.dict(probe.os.environ, {'HOKI_SENSOR_TYPES': 'all', 'HOKI_FLUSH_DIR': '/other',
                                          'HOKI_UNRELATED': '1'}):
            env = probe.recorder_env(Path('/trial'), 26)
        self.assertEqual(env['HOKI_SENSOR_TYPES'], '26')
        self.assertEqual(env['HOKI_FLUSH_DIR'], '/trial/control')
        self.assertNotIn('HOKI_UNRELATED', env)

    def test_cleanup_restores_even_when_deactivation_times_out(self):
        import subprocess
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            probe.save(directory / 'restore.json', {'sensorfwd_was_active': True})
            probe.save(directory / 'activation-intent.json', {'type': 26})
            probe.save(directory / 'trial.json', {'mode': 'suspend', 'type': 26, 'unit': 'test.service'})
            calls = []

            def fake_run(args, **kwargs):
                calls.append(args)
                if args[-1] == 'off':
                    raise subprocess.TimeoutExpired(args, 10)
                return subprocess.CompletedProcess(args, 0)

            with patch.object(probe, 'validate_capture', return_value=directory), \
                 patch.object(probe, 'LOCK', directory / 'lock'), \
                 patch.object(probe, 'run', side_effect=fake_run), \
                 patch.object(probe, 'unit_state', return_value='active'):
                with self.assertRaises(subprocess.TimeoutExpired):
                    probe.cleanup(directory)
            self.assertIn(['systemctl', 'start', 'sensorfwd.service'], calls)
            self.assertTrue((directory / 'cleanup.json').exists())
            self.assertIsNone(json.loads((directory / 'cleanup.json').read_text())['off_rc'])

    def test_cleanup_without_owned_intent_does_nothing(self):
        with tempfile.TemporaryDirectory() as tmp, \
             patch.object(probe, 'validate_capture', return_value=Path(tmp)), \
             patch.object(probe, 'LOCK', Path(tmp) / 'lock'), \
             patch.object(probe, 'run') as command:
            probe.cleanup(tmp)
            command.assert_not_called()

    def test_cleanup_refuses_live_trial_lease(self):
        import fcntl
        with tempfile.TemporaryDirectory() as tmp:
            lock = Path(tmp) / 'lock'
            with lock.open('a') as owner, patch.object(probe, 'LOCK', lock), \
                 patch.object(probe, 'run') as command:
                fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with self.assertRaises(BlockingIOError):
                    probe.cleanup(tmp)
                command.assert_not_called()

    def test_start_requires_explicit_handoff_flag_before_any_probe(self):
        with patch.object(probe.sys, 'argv', ['probe.py', 'launch', '--mode', 'awake', '--action', 'shake']), \
             patch.object(probe, 'preflight') as preflight, \
             patch.object(probe.sys, 'stderr'):
            with self.assertRaises(SystemExit) as error:
                probe.main()
            self.assertEqual(error.exception.code, 2)
            preflight.assert_not_called()


class AnalysisTests(unittest.TestCase):
    def test_acceleration_decode_selects_handle_and_preserves_time(self):
        import struct
        payload = (struct.pack('<fff', 1.25, -2.5, 9.8) + bytes(52)).hex()
        records = analyze.events('4000000000 2000000000 55 1 ' + payload + '\n')
        sample = analyze.acceleration(records, 55)[0]
        self.assertEqual(sample['source_ns'], 2000000000)
        self.assertEqual(sample['arrival_ns'], 4000000000)
        self.assertEqual(sample['x'], 1.25)
        self.assertEqual(sample['y'], -2.5)
        self.assertAlmostEqual(sample['z'], 9.8, places=5)
        self.assertEqual(analyze.acceleration(records, 56), [])
        records[0]['payload_hex'] = (struct.pack('<fff', float('nan'), 0, 0) + bytes(52)).hex()
        with self.assertRaises(ValueError):
            analyze.acceleration(records, 55)

    def fixture(self, directory, alarm=False, residency='3.000', mode='suspend'):
        data = {'trial.json': dict(mode=mode, action='shake'),
                'selected.json': dict(handle=1, type=26),
                'window-start.json': dict(boot_ns=1000000000),
                'window-end.json': dict(boot_ns=5000000000),
                'cleanup.json': dict(restore_rc=0, off_rc=0, sensorfwd_state='active'),
                'finished.json': dict(capture_finished=True),
                'suspend-exit.json': dict(returncode=0)}
        for name, value in data.items():
            probe.save(directory / name, value)
        (directory / 'events.txt').write_text('4000000000 3900000000 1 26 ' + '00' * 64 + '\n')
        (directory / 'recorder.log').write_text('READY boottime_ns=0\nEND events=1\n')
        (directory / 'suspend.log').write_text('RETURN elapsed=4.000s awake=1.000s suspended_estimate=' +
            residency + 's alarm_expired=' + str(alarm).lower() + '\n')

    def test_correlated_event_does_not_prove_wake(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.fixture(Path(tmp))
            report = analyze.summarize(Path(tmp))
        self.assertEqual(report['outcome'], 'early_wake_with_correlated_sensor_event')
        self.assertFalse(report['gesture_wake_proven'])

    def test_alarm_and_failed_suspend_are_not_success(self):
        for alarm, residency, expected in [(True, '3.000', 'fallback_alarm_expired'),
                                          (False, '0.000', 'suspend_not_established')]:
            with tempfile.TemporaryDirectory() as tmp:
                self.fixture(Path(tmp), alarm, residency)
                self.assertEqual(analyze.summarize(Path(tmp))['outcome'], expected)

    def test_stale_event_does_not_count(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            self.fixture(directory)
            (directory / 'events.txt').write_text('4000000000 10 1 26 ' + '00' * 64 + '\n')
            self.assertEqual(analyze.summarize(directory)['outcome'], 'early_wake_without_matching_sensor_event')

    def test_partial_or_bad_cleanup_not_complete(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            self.fixture(directory)
            probe.save(directory / 'cleanup.json', dict(restore_rc=1))
            self.assertEqual(analyze.summarize(directory)['outcome'], 'incomplete_trial')

    def test_truncated_events_rejected(self):
        with self.assertRaises(ValueError):
            analyze.events('4 3 1 26 00')


if __name__ == '__main__':
    unittest.main()
