"""Consumer-policy restart and invalid profile regressions; no hardware access."""
from pathlib import Path
import tempfile
import unittest
from health_broker import Registry

class PolicyTests(unittest.TestCase):
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
