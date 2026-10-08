import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'agent.py'
spec = importlib.util.spec_from_file_location('agent', SOURCE)
a = importlib.util.module_from_spec(spec)
spec.loader.exec_module(a)


def wifi(**changes):
    return dict({'version': 1, 'op': 'save_wifi', 'ssid': 'Cafe ☕',
                 'security': 'wpa-psk', 'password': ' ab\\cd12 ', 'hidden': False}, **changes)


class AgentTests(unittest.TestCase):
    def test_config_preserves_utf8_and_password_without_injection(self):
        name, data = a.wifi_config(wifi(ssid='a\n[evil]'))
        self.assertRegex(name, r'^hokicompanion[0-9a-f]{64}\.config$')
        self.assertIn(b'SSID=610a5b6576696c5d', data)
        self.assertIn(b'Passphrase=\\sab\\\\cd12\\s\n', data)
        self.assertNotIn(b'[evil]', data)

    def test_validation(self):
        for changes in [{'ssid': ''}, {'ssid': 'é'*17}, {'ssid': '\0x'}, {'password': 'short'},
                        {'password': 'abcd\n1234'}, {'password': 'g'*64}, {'security': 'enterprise'},
                        {'hidden': 'false'}, {'password': ['secret']}, {'security': 'open'}]:
            with self.subTest(changes=list(changes)):
                with self.assertRaises(a.RequestError): a.wifi_config(wifi(**changes))
        a.wifi_config(wifi(password='AB'*32))
        _, data = a.wifi_config(wifi(security='open', password=''))
        self.assertNotIn(b'Passphrase', data)

    def test_atomic_private_save_and_update(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)
            self.assertTrue(a.save_wifi(wifi(), path)['saved'])
            files = list(path.iterdir())
            self.assertEqual(len(files), 1)
            self.assertEqual(files[0].stat().st_mode & 0o777, 0o600)
            a.save_wifi(wifi(password='newpassword'), path)
            self.assertEqual(len(list(path.iterdir())), 1)
            self.assertIn('newpassword', files[0].read_text())

    def test_no_symlink_following_on_replacement(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)
            target = path / 'unrelated'; target.write_text('keep')
            name, _ = a.wifi_config(wifi())
            (path/name).symlink_to(target)
            a.save_wifi(wifi(), path)
            self.assertEqual(target.read_text(), 'keep')
            self.assertFalse((path/name).is_symlink())

    def test_existing_provisioner_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)
            existing = path / 'hokipersonal.config'
            existing.write_text('[service_0]\nSSID=' + wifi()['ssid'].encode().hex() + '\nPassphrase=othersecret\n')
            with self.assertRaises(a.RequestError): a.save_wifi(wifi(), path)
            self.assertEqual(len(list(path.iterdir())), 1)
            self.assertIn('othersecret', existing.read_text())

    def test_failure_cleans_temporary_and_preserves_previous(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)
            a.save_wifi(wifi(), path)
            file = next(path.iterdir()); before = file.read_bytes()
            with patch.object(a.os, 'replace', side_effect=OSError):
                with self.assertRaises(OSError): a.save_wifi(wifi(password='newpassword'), path)
            self.assertEqual(file.read_bytes(), before)
            self.assertEqual(list(path.iterdir()), [file])

    def test_missing_battery_not_zero(self):
        with tempfile.TemporaryDirectory() as root, patch.object(a, 'BATTERY', Path(root)):
            self.assertIsNone(a.battery()['percent'])
            (Path(root)/'capacity').write_text('101')
            self.assertIsNone(a.battery()['percent'])
            (Path(root)/'capacity').write_text('0')
            (Path(root)/'status').write_text('Discharging')
            self.assertEqual(a.battery()['percent'], 0)
            self.assertFalse(a.battery()['charging'])

    def test_protocol_and_unknown_fields(self):
        for request in [[], {'version': True, 'op': 'status'}, {'version': 2, 'op': 'status'},
                        {'version': 1, 'op': 'reboot'}, {'version': 1, 'op': 'status', 'shell': 'id'}]:
            with self.assertRaises(a.RequestError): a.handle(request)

    def test_mutations_require_root(self):
        with patch.object(a.os, 'geteuid', return_value=1000):
            with self.assertRaises(a.RequestError): a.handle(wifi())

    def test_time_preserves_ntp_and_accounts_for_processing_delay(self):
        with tempfile.TemporaryDirectory() as root, patch.object(a, 'ZONES', Path(root)):
            (Path(root)/'UTC').write_bytes(b'TZif')
            request = {'utc_ms': 1800000000000, 'timezone': 'UTC'}
            with patch.object(a, 'prop', return_value=True), patch.object(a, 'run') as run, patch.object(a.time, 'clock_settime') as clock:
                self.assertFalse(a.sync_time(request)['clock_adjusted'])
                clock.assert_not_called()
                self.assertEqual(run.call_args.args[-4:], ('SetTimezone', 'sb', 'UTC', 'false'))
            with patch.object(a, 'prop', return_value=False), patch.object(a, 'run'), \
                    patch.object(a.time, 'time', return_value=1700000000), \
                    patch.object(a.time, 'monotonic', side_effect=[10, 13]), patch.object(a.time, 'clock_settime') as clock:
                self.assertTrue(a.sync_time(request)['clock_adjusted'])
                clock.assert_called_once_with(a.time.CLOCK_REALTIME, 1800000003)
            for zone in ['../../etc/passwd', '/etc/passwd', 'Missing', 'UTC;id']:
                with self.assertRaises(a.RequestError): a.sync_time(dict(request, timezone=zone))

    def test_timezone_failure_does_not_adjust_clock(self):
        with patch.object(a, 'prop', return_value=False), patch.object(a, 'run', side_effect=a.RequestError('no')), patch.object(a.time, 'clock_settime') as clock:
            with self.assertRaises(a.RequestError): a.sync_time({'utc_ms':1800000000000, 'timezone':'Europe/Berlin'})
            clock.assert_not_called()

    def test_protocol_subprocess_never_echoes_bad_request(self):
        for raw in [b'x' * 8193 + b'\n', b'{"password":"secret"}\n', b'invalid\n', b'{"version":1,"op":[]}\n']:
            p = subprocess.run([sys.executable, str(SOURCE)], input=raw, capture_output=True)
            self.assertEqual(p.returncode, 0)
            self.assertFalse(json.loads(p.stdout)['ok'])
            self.assertNotIn(b'secret', p.stdout + p.stderr)

if __name__ == '__main__': unittest.main()
