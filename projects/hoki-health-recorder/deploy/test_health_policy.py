"""Host-only profile lifecycle tests; never contact systemd or the watch."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('health_policy', Path(__file__).with_name('health-policy.py'))
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class PolicyTests(unittest.TestCase):
    def exercise(self, profiles, fail_start=False):
        calls = []
        class Client:
            def request(self, command, **kwargs):
                if command == 'status':
                    return {'config': {'sensor_profile': profiles[ticks]}}
                calls.append((command, kwargs))
            def inhibit(self, reason):
                calls.append(('inhibit', reason))
            def close(self):
                calls.append(('close',))
        ticks = 0
        def sleep(_seconds):
            nonlocal ticks
            ticks += 1
            if ticks == len(profiles):
                policy.stopping = True
        def systemctl(*args):
            calls.append(args)
            if fail_start and args[0] == 'start':
                raise RuntimeError('admission failed')
            return 'inactive'
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(policy, 'ENV', Path(directory) / 'profile.env'), \
                patch.object(policy, 'STATUS', Path(directory) / 'status.json'), \
                patch.object(policy, 'PowerClient', Client), \
                patch.object(policy, 'systemctl', systemctl), \
                patch.object(policy.time, 'sleep', sleep):
            policy.stopping = False
            policy.run()
        return calls

    def test_stopped_collection_does_not_restart_until_profile_changes(self):
        calls = self.exercise(['daily', 'daily', 'daily', 'sleep', 'off'])
        self.assertEqual([c for c in calls if c[0] == 'start'], [('start', policy.UNIT)] * 2)
        self.assertFalse(any('hoki-health-recording.service' in c for c in calls))

    def test_failed_start_is_not_retried_on_reconnect(self):
        calls = self.exercise(['full', 'full', 'full'], fail_start=True)
        self.assertEqual([c for c in calls if c[0] == 'start'], [('start', policy.UNIT)])
        self.assertTrue(any(c[0] == 'close' for c in calls))

    def test_superseded_profiles_are_not_started_or_published(self):
        for change_during in ('stop', 'start'):
            live = {'profile': 'full'}
            calls, published = [], []
            class Client:
                def request(self, command, **kwargs):
                    return {'config': {'sensor_profile': live['profile']}}
                def inhibit(self, reason): pass
                def close(self): pass
            def systemctl(*args):
                calls.append(args[0])
                if args[0] == change_during:
                    live['profile'] = 'off'
                return 'inactive'
            def sleep(_): policy.stopping = True
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(policy, 'ENV', Path(directory) / 'profile.env'), \
                    patch.object(policy, 'PowerClient', Client), \
                    patch.object(policy, 'systemctl', systemctl), \
                    patch.object(policy, 'publish', lambda profile, error=None: published.append(profile)), \
                    patch.object(policy.time, 'sleep', sleep):
                policy.stopping = False
                policy.run()
            self.assertEqual(calls.count('start'), 0 if change_during == 'stop' else 1)
            self.assertEqual(published, ['off'])
            if change_during == 'start':
                self.assertEqual(calls[calls.index('start') + 1], 'stop')

    def test_shutdown_during_stop_never_starts_collection(self):
        calls = []
        class Client:
            def request(self, *args, **kwargs): return {'config': {'sensor_profile': 'full'}}
            def inhibit(self, reason): pass
            def close(self): pass
        def systemctl(*args):
            calls.append(args[0])
            policy.stopping = True
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(policy, 'ENV', Path(directory) / 'profile.env'), \
                patch.object(policy, 'PowerClient', Client), \
                patch.object(policy, 'systemctl', systemctl):
            policy.stopping = False
            policy.run()
        self.assertNotIn('start', calls)

    def test_unknown_profiles_are_rejected(self):
        with self.assertRaises(RuntimeError):
            policy.desired({'config': {'sensor_profile': 'unknown'}})


if __name__ == '__main__':
    unittest.main()
