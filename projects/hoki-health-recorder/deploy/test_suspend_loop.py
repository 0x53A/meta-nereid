"""Run the actual coordinator with isolated fake commands; never touch sysfs."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('suspend-loop.sh').resolve()
PARENT = 'hoki-recording-power-12345678-1234-1234-1234-123456789abc.service'
HAL = 'hal.service'
SSC = 'ssc.service'
MOCK = r'''
import json, os, sys
from pathlib import Path
root = Path(os.environ['MOCK_ROOT'])
state = json.loads((root/'state').read_text())
cmd = Path(sys.argv[0]).name
args = sys.argv[1:]
with (root/'calls').open('a') as f: f.write(json.dumps([cmd] + args)+'\n')
rc = 0
if cmd == 'id': print(0)
elif cmd == 'cat':
    if args[0] in state['read_failures']: sys.exit(1)
    names = {'/sys/class/power_supply/battery/status': state['battery'],
             '/sys/class/android_usb/android0/state': state['usb'],
             '/proc/sys/kernel/random/uuid': '12345678-1234-1234-1234-123456789abd'}
    if args[0] not in names: sys.exit(93)
    print(names[args[0]])
elif cmd == 'systemctl':
    if args[0] == 'show':
        key = args[args.index('-p')+1]
        print({'MainPID': os.environ['MOCK_MAINPID'], 'ActiveState': 'active',
               'BindsTo': state['binds']}[key])
    elif args[0] == 'is-active':
        if args[-1] == 'hal.service':
            state['loops'] += 1
        rc = 0 if state['loops'] <= state['active_loops'] else 3
    else: rc = 94
elif cmd == 'systemd-run':
    rc = state['child_rc'].pop(0) if isinstance(state['child_rc'], list) else state['child_rc']
elif cmd == 'sleep': pass
else: rc = 95
(root/'state').write_text(json.dumps(state))
sys.exit(rc)
'''


class Coordinator(unittest.TestCase):
    def run_loop(self, *, battery='Discharging', usb='DISCONNECTED', child_rc=0,
                 active_loops=2, binds=f'{HAL} {SSC}', read_failures=()):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            mock = root/'mock'
            mock.write_text(f'#!{sys.executable}\n' + MOCK)
            mock.chmod(0o700)
            for name in ['id', 'cat', 'systemctl', 'systemd-run', 'sleep']:
                (root/name).symlink_to(mock)
            (root/'state').write_text(json.dumps(dict(battery=battery, usb=usb,
                child_rc=child_rc, active_loops=active_loops, loops=0, binds=binds,
                read_failures=read_failures)))
            env = dict(os.environ, PATH=str(root), MOCK_ROOT=str(root), HOKI_POWER_SUPERVISOR=PARENT)
            # exec preserves PID, allowing the real script's self-identity check.
            result = subprocess.run(['/bin/sh', '-c',
                'MOCK_MAINPID=$$; export MOCK_MAINPID; exec /bin/sh "$@"',
                'wrapper', str(SCRIPT), '/controller', '/socket', '/capture', HAL, SSC],
                env=env, capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in (root/'calls').read_text().splitlines()]
            return result, calls

    def test_charger_and_connected_usb_defer_without_suspend(self):
        for battery, usb in [('Full', 'CONFIGURED'), ('Discharging', 'CONFIGURED'),
                             ('Unknown', 'DISCONNECTED'), ('', 'DISCONNECTED'),
                             ('Discharging', '')]:
            result, calls = self.run_loop(battery=battery, usb=usb)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(any(c[0] == 'systemd-run' for c in calls))
            self.assertEqual([c for c in calls if c[0] == 'sleep'], [['sleep', '30']]*2)

    def test_failed_telemetry_read_stops_before_suspend_or_retry(self):
        for path in ('/sys/class/power_supply/battery/status',
                     '/sys/class/android_usb/android0/state'):
            with self.subTest(path=path):
                result, calls = self.run_loop(read_failures=[path], active_loops=20)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(c[0] in ('systemd-run', 'sleep') for c in calls))
                self.assertEqual(calls[-1], ['cat', path])

    def test_successful_attempts_are_bounded_and_bound_to_all_owners(self):
        result, calls = self.run_loop()
        self.assertEqual(result.returncode, 0, result.stderr)
        runs = [c for c in calls if c[0] == 'systemd-run']
        self.assertEqual(len(runs), 2)
        for run in runs:
            for option in ['--wait', '--collect', '--property=Type=exec',
                           '--property=RuntimeMaxSec=30', '--property=TimeoutStopSec=5',
                           '--property=KillMode=control-group',
                           f'--property=BindsTo={PARENT} {HAL} {SSC}',
                           f'--property=After={PARENT} {HAL} {SSC}']:
                self.assertIn(option, run)
            self.assertEqual(run[-4:], ['/controller', '--suspend-recording-paced', '/socket', '/capture'])
        self.assertEqual([c for c in calls if c[0] == 'sleep'], [])

    def test_healthy_deferral_or_short_return_keeps_cooldown(self):
        result, calls = self.run_loop(child_rc=77, active_loops=4)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c for c in calls if c[0] == 'sleep'], [['sleep', '1']]*4)
        self.assertEqual(result.stdout.count('POWER_COOLDOWN'), 4)

    def test_cooldown_resets_retry_and_failure_streaks(self):
        result, calls = self.run_loop(child_rc=[1] + [76]*9 + [77, 1, 76], active_loops=13)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c for c in calls if c == ['sleep', '5']], [['sleep', '5']]*2)
        self.assertFalse(any(c == ['sleep', '10'] for c in calls))

    def test_three_failures_stop_with_backoff(self):
        result, calls = self.run_loop(child_rc=1, active_loops=10)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(sum(c[0] == 'systemd-run' for c in calls), 3)
        self.assertEqual([c for c in calls if c[0] == 'sleep'], [['sleep', '5'], ['sleep', '10']])

    def test_missing_recorder_binding_refuses_any_attempt(self):
        result, calls = self.run_loop(binds=HAL)
        self.assertEqual(result.returncode, 64)
        self.assertFalse(any(c[0] in ('cat', 'systemd-run', 'sleep') for c in calls))

    def test_retry_conditions_get_short_delay_but_are_bounded(self):
        result, calls = self.run_loop(child_rc=76, active_loops=20)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(sum(c[0] == 'systemd-run' for c in calls), 10)
        self.assertEqual([c for c in calls if c[0] == 'sleep'], [['sleep', '1']]*9)

    def test_retry_does_not_erase_prior_hard_failures(self):
        result, calls = self.run_loop(child_rc=[1, 76, 1, 76, 1], active_loops=5)
        self.assertEqual(result.returncode, 1)
        self.assertEqual([c for c in calls if c[0] == 'sleep'],
                         [['sleep', '5'], ['sleep', '1'], ['sleep', '10'], ['sleep', '1']])

    def test_success_resets_retry_count(self):
        result, calls = self.run_loop(child_rc=[76]*9 + [0, 76], active_loops=11)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sum(c[0] == 'systemd-run' for c in calls), 11)

    def test_success_resets_consecutive_failure_count(self):
        result, calls = self.run_loop(child_rc=[1, 0, 1, 1], active_loops=4)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sum(c[0] == 'systemd-run' for c in calls), 4)
        self.assertEqual([c for c in calls if c[0] == 'sleep'],
                         [['sleep', '5'], ['sleep', '5'], ['sleep', '10']])


if __name__ == '__main__':
    unittest.main()
