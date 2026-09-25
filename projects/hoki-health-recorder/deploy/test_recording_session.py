import configparser
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('recording_session', Path(__file__).with_name('recording-session.py'))
service = importlib.util.module_from_spec(spec)
spec.loader.exec_module(service)
MIB = 1024 * 1024


class Sessions(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.calls = []
        self.overrides = [patch.object(service, name, root / value) for name, value in
                          [('STATE', 'archives'), ('RUNTIME', 'runtime'), ('DROPIN', 'units/record.conf')]]
        for override in self.overrides:
            override.start()
        self.commands = patch.object(service, 'command', side_effect=self.command)
        self.commands.start()
        self.battery = patch.object(service, 'battery', return_value={'status': 'Discharging', 'capacity': '80'})
        self.battery.start()
        space = os.statvfs(root)
        self.space = patch.object(service.os, 'statvfs', return_value=space)
        self.space.start()

    def tearDown(self):
        self.space.stop()
        self.battery.stop()
        self.commands.stop()
        for override in reversed(self.overrides):
            override.stop()
        self.tmp.cleanup()

    def command(self, *args, **kwargs):
        self.calls.append(args)
        if args[:2] == ('systemctl', 'show'):
            return ''
        if 'loadPlugin' in args:
            return 'b true'
        if 'requestSensor' in args:
            session = service.load_session()
            with socket.socket(socket.AF_UNIX) as sock:
                sock.bind(str(service.socket_path(session)))
            return 'i 1'
        return ''

    def test_two_cycles_preserve_separate_archives_and_restore(self):
        ids = []
        for _ in range(2):
            service.prepare()
            session = service.load_session()
            ids.append(session['id'])
            session['phase'] = 'stopped'
            service.persist(session)
            service.cleanup()
            self.assertFalse(service.DROPIN.exists())
            self.assertTrue(json.loads((service.archive(session) / 'session.json').read_text())['sensorfw_restored'])
            # systemd removes RuntimeDirectory after ExecStopPost.
            shutil.rmtree(service.RUNTIME)
        self.assertNotEqual(*ids)
        self.assertTrue(all((service.STATE / identity).is_dir() for identity in ids))
        self.assertEqual(sum('restart' in command for command in self.calls), 4)

    def test_cleanup_restarts_are_not_ordered_against_own_stop_job(self):
        service.prepare()
        self.calls.clear()
        service.cleanup()
        restarted = {args[2] for args in self.calls if args[:2] == ('systemctl', 'restart')}
        unit = configparser.ConfigParser(interpolation=None)
        unit.read(Path(__file__).with_name('hoki-health-recording.service'))
        ordered = set(unit['Unit'].get('After', '').split() + unit['Unit'].get('Before', '').split())
        self.assertTrue(restarted)
        self.assertFalse(restarted & ordered, 'ExecStopPost must not wait for a job ordered after its own stop')

    def test_foreign_recording_is_not_restarted(self):
        with patch.object(service, 'command', return_value='HOKI_RECORDING_SOCKET=/other/control'):
            with self.assertRaisesRegex(RuntimeError, 'Another recorder'):
                service.prepare()
        service.cleanup()
        self.assertFalse(service.DROPIN.exists())
        self.assertEqual(self.calls, [])

    def test_foreign_override_is_not_removed(self):
        service.prepare()
        service.DROPIN.write_text('another owner')
        count = len(self.calls)
        with self.assertRaisesRegex(RuntimeError, 'another sensorfw'):
            service.cleanup()
        self.assertEqual(service.DROPIN.read_text(), 'another owner')
        self.assertEqual(len(self.calls), count)

    def test_failed_prepare_still_restores_owned_configuration(self):
        def fail_restart(*args, **kwargs):
            if 'restart' in args:
                raise subprocess.CalledProcessError(1, args)
            return self.command(*args, **kwargs)
        with patch.object(service, 'command', side_effect=fail_restart):
            with self.assertRaises(subprocess.CalledProcessError):
                service.prepare()
        self.assertTrue(service.DROPIN.exists())
        service.cleanup()
        self.assertFalse(service.DROPIN.exists())
        self.assertEqual(service.load_session()['phase'], 'interrupted')

    def test_low_battery_rejects_before_configuration(self):
        with patch.object(service, 'battery', return_value={'status': 'Discharging', 'capacity': '15'}):
            with self.assertRaisesRegex(RuntimeError, 'Charge'):
                service.prepare()
        self.assertFalse(service.DROPIN.exists())
        self.assertFalse((service.RUNTIME / 'session.json').exists())

    def test_budget_preserves_reserve_and_caps_capture(self):
        with self.assertRaises(RuntimeError):
            service.capture_budget((256 + 16 + 127) * MIB)
        self.assertEqual(service.capture_budget((256 + 16 + 128) * MIB), 128 * MIB)
        self.assertEqual(service.capture_budget(600 * MIB), 328 * MIB)
        self.assertEqual(service.capture_budget(4096 * MIB), 1024 * MIB)

    def test_monitor_stops_controller_on_low_battery(self):
        service.prepare()

        class Child:
            pid = 999
            code = None
            def poll(self): return self.code
            def send_signal(self, _signal): self.code = 0
            def wait(self, **_kwargs): return self.code

        child = Child()
        with patch.object(service.subprocess, 'Popen', return_value=child), \
             patch.object(service.signal, 'signal'), \
             patch.object(service, 'battery', return_value={'status': 'Discharging', 'capacity': '15'}):
            self.assertEqual(service.run(), 0)
        self.assertEqual(child.code, 0)
        self.assertEqual(service.load_session()['stop_reason'], 'low_battery')


if __name__ == '__main__':
    unittest.main()
