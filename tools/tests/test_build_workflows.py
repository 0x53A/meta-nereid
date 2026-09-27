"""Local regression coverage for build orchestration; no remote build required."""
import importlib.util
import itertools
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('workspace_runner', TOOLS / 'test-runtime-workspace.py')
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class WorkspaceRunnerTest(unittest.TestCase):
    def run_fixture(self, command_transform=lambda text: text):
        recipe = (TOOLS.parent / 'recipes-hoki/hoki-ui/hoki-ui_1.0.bb').read_text()
        body = recipe.split('do_compile() {', 1)[1].split('\n}', 1)[0]
        body = command_transform(body.replace('${RUST_HOST_SYS}', 'arm-poky-linux-gnueabi'))
        with tempfile.TemporaryDirectory() as tmp:
            build = Path(tmp)
            compile_script = build / 'tmp/work/hoki-poky-linux-gnueabi/hoki-ui/1.0/temp/run.do_compile'
            compile_script.parent.mkdir(parents=True)
            compile_script.write_text('do_compile() {\n' + body + '\n}\ndo_compile\n')
            qemu = build / 'tmp/sysroots-components/x86_64/qemu-native/usr/bin/qemu-arm'
            qemu.parent.mkdir(parents=True)
            qemu.touch()
            (build / 'tmp/work/hoki-poky-linux-gnueabi/asteroid-image/1.0/rootfs').mkdir(parents=True)
            scripts = []
            def capture(args, **kwargs):
                scripts.append(Path(args[1]).read_text())
            with patch('sys.argv', ['runner', tmp]), \
                    patch.object(runner.shutil, 'which', return_value='/fixture/dbus-daemon'), \
                    patch.object(runner.subprocess, 'run', side_effect=capture):
                runner.main()
            self.assertEqual(len(scripts), 1)
            return scripts[0]

    def test_current_recipe_runs_tests_and_preserves_sbom(self):
        script = self.run_fixture()
        self.assertIn('cargo test --no-fail-fast -v --frozen --release -Z sbom --target arm-poky-linux-gnueabi', script)
        self.assertIn('--workspace "$@" -- --test-threads=1', script)
        self.assertIn('CARGO_TARGET_ARM_POKY_LINUX_GNUEABI_RUNNER=', script)
        self.assertNotIn('cargo build ', script)

    def test_pre_sbom_compile_script_still_supported(self):
        script = self.run_fixture(lambda body: body.replace(' -Z sbom', ''))
        self.assertIn('--release --target arm-poky-linux-gnueabi', script)
        self.assertNotIn('-Z sbom', script)

    def test_unexpected_command_is_rejected(self):
        with self.assertRaisesRegex(SystemExit, 'Unexpected BitBake compile script'):
            self.run_fixture(lambda body: body.replace(' -Z sbom', ' -Z unknown'))


class CargoReportGateTest(unittest.TestCase):
    def test_all_feature_combinations_with_and_without_reports(self):
        wrapper = (TOOLS / 'build-hoki.sh').read_text()
        # Execute the actual post-download gate, stopping before SDK transfers.
        gate = wrapper.split('# The Cargo precursors live', 1)[1].split('\necho ""', 1)[0]
        gate = '# The Cargo precursors live' + gate
        with tempfile.TemporaryDirectory() as tmp:
            for reports in (False, True):
                if reports:
                    (Path(tmp) / 'cargo-sbom').mkdir()
                for flags in itertools.product(('0', '1'), repeat=3):
                    with self.subTest(reports=reports, flags=flags):
                        env = dict(os.environ, image_dir=tmp)
                        env.update(zip(('HOKI_CUSTOM_UI', 'HOKI_BLE_SSH', 'HOKI_ACOUSTIC_SSH'), flags))
                        result = subprocess.run(['bash', '-ec', gate + '\necho reached-sdk-download'],
                                                env=env, capture_output=True, text=True)
                        allowed = reports or flags == ('0', '0', '0')
                        self.assertEqual(result.returncode, 0 if allowed else 1, result.stderr)
                        self.assertEqual('reached-sdk-download' in result.stdout, allowed)
                        if not allowed:
                            self.assertIn('Missing Cargo SBOM reports', result.stderr)


if __name__ == '__main__':
    unittest.main()
