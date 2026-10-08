import importlib.util
import ctypes
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('wifi_sleep', Path(__file__).with_name('wifi-sleep.py'))
wifi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wifi)


class WifiSleepTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        wifi.STATE = root / 'state' / 'state.json'
        wifi.INTERFACE = root / 'wlan0'
        wifi.INTERFACE.mkdir()
        (wifi.INTERFACE / 'flags').write_text('0x1003')
        (wifi.INTERFACE / 'ifindex').write_text('7')
        (root / 'phy0').mkdir()
        (wifi.INTERFACE / 'phy80211').symlink_to(root / 'phy0')
        self.enabled = False
        self.calls = []

    def command(self, *args):
        self.calls.append(args)
        if args[-1] == 'show':
            return 'WoWLAN is enabled:\n * wake up on special any trigger\n' if self.enabled else 'WoWLAN is disabled.\n'
        if args[-1] == 'any':
            self.enabled = True
        if args[-1] == 'disable':
            self.enabled = False
        return ''

    def test_normal_cycle_and_idempotent_restore(self):
        with patch.object(wifi, 'run', self.command), patch.object(wifi, 'driver') as driver:
            wifi.prepare()
            self.assertTrue(self.enabled)
            self.assertTrue(wifi.STATE.exists())
            wifi.restore()
            wifi.restore()
            self.assertFalse(self.enabled)
            self.assertFalse(wifi.STATE.exists())
            self.assertEqual([c.args[0] for c in driver.call_args_list], ['quiet', 'resume'])

    def test_existing_configuration_is_not_overwritten(self):
        self.enabled = True
        with patch.object(wifi, 'run', self.command), patch.object(wifi, 'driver'):
            wifi.prepare()
            wifi.restore()
        self.assertTrue(self.enabled)
        self.assertTrue(all(c[-1] == 'show' for c in self.calls))

    def test_failed_quiet_restores_wowlan_and_marker(self):
        with patch.object(wifi, 'run', self.command), patch.object(wifi, 'driver', side_effect=[RuntimeError('quiet failed'), None]):
            with self.assertRaisesRegex(RuntimeError, 'quiet failed'):
                wifi.prepare()
        self.assertFalse(self.enabled)
        self.assertFalse(wifi.STATE.exists())

    def test_failed_restore_is_retained_and_retried_before_preparation(self):
        with patch.object(wifi, 'run', self.command), patch.object(wifi, 'driver'):
            wifi.prepare()
        with patch.object(wifi, 'run', self.command), patch.object(wifi, 'driver', side_effect=RuntimeError('resume failed')):
            with self.assertRaisesRegex(RuntimeError, 'resume failed'):
                wifi.restore()
            self.assertTrue(wifi.STATE.exists())
            with self.assertRaisesRegex(RuntimeError, 'resume failed'):
                wifi.prepare()
        with patch.object(wifi, 'run', self.command), patch.object(wifi, 'driver'):
            wifi.restore()
        self.assertFalse(wifi.STATE.exists())

    def test_down_interface_does_not_touch_radio(self):
        (wifi.INTERFACE / 'flags').write_text('0x1002')
        with patch.object(wifi, 'run') as command, patch.object(wifi, 'driver') as driver:
            wifi.prepare()
            command.assert_not_called()
            driver.assert_not_called()

    def test_replaced_interface_keeps_recovery_evidence(self):
        with patch.object(wifi, 'run', self.command), patch.object(wifi, 'driver'):
            wifi.prepare()
            (wifi.INTERFACE / 'ifindex').write_text('8')
            with self.assertRaisesRegex(RuntimeError, 'identity changed'):
                wifi.restore()
        self.assertTrue(wifi.STATE.exists())

    def test_unknown_status_refuses_mutation(self):
        with patch.object(wifi, 'run', return_value='unexpected output'), patch.object(wifi, 'driver') as driver:
            with self.assertRaisesRegex(RuntimeError, 'unrecognized'):
                wifi.prepare()
            driver.assert_not_called()
        self.assertFalse(wifi.STATE.exists())

    def test_private_ioctl_layout_and_fixed_commands(self):
        commands = []

        def ioctl(fd, number, request, mutate):
            self.assertEqual(number, 0x89f1)
            self.assertTrue(mutate)
            self.assertEqual(len(request), 40 if wifi.struct.calcsize('P') == 8 else 32)
            name, pointer = wifi.struct.unpack_from('@16sP', request)
            self.assertEqual(name.rstrip(b'\0'), b'wlan0')
            raw = ctypes.string_at(pointer, wifi.struct.calcsize('@Pii'))
            buffer, used, size = wifi.struct.unpack('@Pii', raw)
            self.assertEqual(used, 0)
            commands.append(ctypes.string_at(buffer, size))
            return 0

        with patch.object(wifi.fcntl, 'ioctl', ioctl):
            wifi.quiet(True)
            wifi.quiet(False)
        self.assertEqual(commands, [b'SETSUSPENDMODE 1\0', b'SETSUSPENDMODE 0\0'])


if __name__ == '__main__':
    unittest.main()
