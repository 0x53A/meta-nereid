"""Consumer-policy restart and invalid profile regressions; no hardware access."""
from pathlib import Path
import tempfile
import unittest
from health_broker import Registry, cleanly_stopped, stopped_status

class PolicyTests(unittest.TestCase):
    def test_policy_reports_clean_stop_without_restarting_or_claiming_ready(self):
        from unittest.mock import patch
        import health_broker as b
        ticks = [0]
        calls = []
        requests = []
        holder = {}
        class Power:
            def request(self, command, **fields):
                requests.append((command, fields))
                return {'config':{'sensor_profile':'full'}, 'generation':7}
            def inhibit(self, reason):
                requests.append(('inhibit-start', reason))
            def close(self): pass
        class Server:
            def shutdown(self): pass
            def server_close(self): pass
        def systemctl(*args):
            calls.append(args)
            return 'active' if ticks[0] == 0 and any(c[0] == 'start' for c in calls) else 'inactive'
        def serve(registry):
            holder['registry'] = registry
            return Server()
        session = dict(id='capture', phase='stopped', controller_exit=0, sensorfw_restored=True,
                       finished_boottime_seconds=100, stop_reason='low_battery')
        with tempfile.TemporaryDirectory() as d, patch.object(b,'RUNTIME',Path(d)), \
             patch.object(b,'Registry',lambda: Registry(Path(d)/'plan.json')), \
             patch.object(b,'serve',serve), patch.object(b,'recording_session',lambda state: session if state=='inactive' else {}), \
             patch.object(b.time,'sleep',lambda _:ticks.__setitem__(0,ticks[0]+1)):
            b.run_policy(systemctl,Power,lambda:ticks[0]>=3)
        self.assertEqual(sum(c[0]=='start' for c in calls), 1)
        self.assertIn(('sensor-idle',dict(idle=True,profile='full',generation=7)), requests)
        snapshot = holder['registry'].snapshot()
        self.assertFalse(snapshot['ready'])
        self.assertFalse(snapshot['recording'])
        self.assertEqual(snapshot['stop_reason'], 'low_battery')

    def test_clean_completion_requires_finalization_and_restored_sensorfw(self):
        session = dict(phase='stopped', controller_exit=0, sensorfw_restored=True,
                       finished_boottime_seconds=100, stop_reason='low_battery')
        self.assertTrue(cleanly_stopped('inactive', session))
        for state in ['active', 'activating', 'deactivating', 'failed']:
            self.assertFalse(cleanly_stopped(state, session))
        for fields in [dict(phase='failed'), dict(controller_exit=1), dict(sensorfw_restored=False),
                       dict(cleanup_error='failed'), dict(monitor_error='failed'),
                       dict(finished_boottime_seconds=None)]:
            self.assertFalse(cleanly_stopped('inactive', dict(session, **fields)))

    def test_stopped_status_does_not_claim_recording_or_readiness(self):
        result = stopped_status('inactive', {'stop_reason':'low_battery'})
        self.assertFalse(result['ready'])
        self.assertFalse(result['recording'])
        self.assertIn('low battery', result['error'])
        self.assertIsNone(stopped_status('active', {}))

    def test_final_state_survives_removed_runtime_but_not_a_new_boot(self):
        import json
        from unittest.mock import patch
        import health_broker as b
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            final = root/'final.json'
            boot = root/'boot'
            final.write_text(json.dumps(dict(boot_id='old', phase='stopped')))
            boot.write_text('old\n')
            with patch.object(b,'SESSION',root/'absent'), patch.object(b,'FINAL_SESSION',final), patch.object(b,'BOOT_ID',boot):
                self.assertEqual(b.recording_session('inactive')['phase'], 'stopped')
                self.assertEqual(b.recording_session('active'), {})
                boot.write_text('new\n')
                self.assertEqual(b.recording_session('inactive'), {})

    def test_restart_changes_epoch_even_if_revision_and_profile_match(self):
        with tempfile.TemporaryDirectory() as d:
            a=Registry(Path(d)/'plan.json');a.settings('full')
            b=Registry(Path(d)/'plan.json');b.settings('full')
            self.assertEqual(a.revision,b.revision)
            self.assertNotEqual(a.epoch,b.epoch)
            self.assertFalse(b.snapshot()['ready'])
    def test_unknown_settings_profile_is_rejected_without_mutation(self):
        with tempfile.TemporaryDirectory() as d:
            registry=Registry(Path(d)/'plan.json');registry.settings('daily')
            before=registry.snapshot()
            for invalid in ['unknown','running','spo2']:
                with self.assertRaises(ValueError):registry.settings(invalid)
            self.assertEqual(registry.snapshot(),before)
