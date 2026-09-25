import copy
import tempfile
import unittest
from pathlib import Path
from session_units import render, save_bundle


class SessionUnits(unittest.TestCase):
    def setUp(self):
        self.owner = "12345678-1234-1234-1234-123456789abc"
        self.runtime = dict(version=1, owner=self.owner, boot_id=self.owner,
                            duration_seconds=28800, controller="/opt/health/controller",
                            helper="/opt/health/collector", session="/var/lib/health/profile",
                            apply_unit=f"hoki-sleep-{self.owner}.service",
                            restore_unit=f"hoki-sleep-restore-{self.owner}-{self.owner}.service")
        self.selected = dict(boot_id=self.owner, endpoints={name: "09" + "00"*8 + "11" + "01"*8
                             for name in ("fsl_min", "fsl_sleep", "fsl_rhr")})

    def make(self, runtime=None, selected=None, root="/var/lib/health/recording"):
        return render(runtime or self.runtime, selected or self.selected, root,
                      "/run/health/control", "/opt/health/suspend-loop.sh", 7200)

    def test_power_failure_and_recovery_ordering(self):
        units, manifest = self.make()
        names = manifest["units"]
        bindings = units[names["target"]].split("BindsTo=", 1)[1].splitlines()[0].split()
        self.assertEqual(set(bindings), {self.runtime['apply_unit'], names['hal'], names['ssc'], names['power']})
        for name in ('hal', 'ssc', 'power', 'gate'):
            self.assertIn(f"PartOf={names['target']}\n", units[names[name]])
        restore = units[self.runtime['restore_unit']+'.d/50-recording.conf']
        for name in ('hal', 'ssc', 'gate', 'power'):
            self.assertIn(names[name], restore)
        self.assertIn('RuntimeMaxSec=28920\n', units[names['hal']])
        self.assertIn('RuntimeMaxSec=28920\n', units[names['ssc']])
        self.assertIn('SSC_RHR_SUID=', units[names['ssc']])

    def test_custom_budget_reaches_admission_and_hal(self):
        units, manifest = render(self.runtime, self.selected, '/var/lib/health/recording',
                                 '/run/health/control', '/opt/health/suspend-loop.sh',
                                 7200, 536870912)
        for text in [units[manifest['units']['hal']],
                     units[self.runtime['apply_unit'] + '.d/50-recording.conf']]:
            self.assertIn('Environment=HOKI_HAL_LIMIT_BYTES=536870912\n', text)
        self.assertEqual(manifest['hal_limit_bytes'], 536870912)

    def test_space_admission_runs_before_profile_setters(self):
        units, manifest = self.make()
        dropin = units[self.runtime['apply_unit'] + '.d/50-recording.conf']
        self.assertIn('ExecStartPre=/opt/health/controller '
                      '--check-recording-space /var/lib/health/recording '
                      '/var/lib/health/recording/ssc\n', dropin)
        # A failed pre-start command must fail the unit, never be ignored.
        self.assertNotIn('ExecStartPre=-', dropin)
        self.assertIn('After=' + self.runtime['apply_unit'],
                      units[manifest['units']['gate']])

    def test_rejects_stale_endpoints_unsafe_paths_and_unbounded_duration(self):
        selected = copy.deepcopy(self.selected)
        selected['boot_id'] = 'other'
        with self.assertRaises(ValueError): self.make(selected=selected)
        for value in (0, 86401, True, '28800'):
            runtime = dict(self.runtime, duration_seconds=value)
            with self.assertRaises(ValueError): self.make(runtime=runtime)
        for value in ('/tmp/../health', '/tmp/x\nExecStart=/bin/false', '/tmp//health', '/tmp/%n', '/var/lib/health/profile'):
            with self.assertRaises(ValueError): self.make(root=value)
        selected = copy.deepcopy(self.selected)
        del selected['endpoints']['fsl_rhr']
        with self.assertRaises(ValueError): self.make(selected=selected)

    def test_exclusive_private_staging(self):
        with tempfile.TemporaryDirectory() as temp:
            destination = Path(temp)/'bundle'
            units, manifest = self.make()
            save_bundle(destination, units, manifest)
            self.assertEqual(destination.stat().st_mode & 0o777, 0o700)
            self.assertEqual((destination/'recording.json').stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError): save_bundle(destination, units, manifest)

    def test_optional_workout_selection_is_explicit_and_validated(self):
        units, manifest = self.make()
        self.assertFalse(manifest['workout_summary_enabled'])
        selected = copy.deepcopy(self.selected)
        selected['endpoints']['fsl_wk'] = selected['endpoints']['fsl_rhr']
        units, manifest = self.make(selected=selected)
        self.assertTrue(manifest['workout_summary_enabled'])
        self.assertIn('Environment=SSC_WORKOUT_SUID=', units[manifest['units']['ssc']])
        selected['endpoints']['fsl_wk'] = 'invalid'
        with self.assertRaises(ValueError): self.make(selected=selected)


if __name__ == '__main__':
    unittest.main()
