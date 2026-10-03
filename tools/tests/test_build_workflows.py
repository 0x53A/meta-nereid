"""Local regression coverage for build orchestration; no remote build required."""
import importlib.util
import itertools
import os
from pathlib import Path
import shutil
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


class LocalContainerTest(unittest.TestCase):
    def invoke(self, runtime, exit_code=0):
        with tempfile.TemporaryDirectory() as tmp:
            executable = Path(tmp) / runtime
            executable.write_text(f'#!{shutil.which("bash")}\nprintf "%s\\n" "$@"\n'
                                  f'exit {exit_code}\n')
            executable.chmod(0o755)
            env = dict(os.environ, PATH=tmp + os.pathsep + os.environ['PATH'])
            return subprocess.run(
                ['bash', str(TOOLS / 'run-build-container.sh'), runtime,
                 '/tmp/build directory', 'bash', '-c', 'echo "two words"'],
                env=env, capture_output=True, text=True)

    def test_docker_preserves_arguments_and_host_ownership(self):
        result = self.invoke('docker')
        self.assertEqual(result.returncode, 0, result.stderr)
        args = result.stdout.splitlines()
        self.assertEqual(args[args.index('--user') + 1], f'{os.getuid()}:{os.getgid()}')
        self.assertIn('/tmp/build directory:/asteroid:z', args)
        self.assertEqual(args[-3:], ['bash', '-c', 'echo "two words"'])
        self.assertNotIn('--userns', args)

    def test_podman_keeps_existing_user_namespace_behavior(self):
        result = self.invoke('podman')
        self.assertEqual(result.returncode, 0, result.stderr)
        args = result.stdout.splitlines()
        self.assertEqual(args[args.index('--userns') + 1], 'keep-id')
        self.assertNotIn('--user', args)

    def test_container_failure_is_reported(self):
        self.assertEqual(self.invoke('docker', exit_code=23).returncode, 23)

    def test_source_workspace_cannot_be_used_as_staging(self):
        workspace = TOOLS.parent.parent
        for directory in (workspace, workspace / 'build', workspace.parent):
            with self.subTest(directory=directory):
                env = dict(os.environ, NEREID_BUILD_HOST='local',
                           NEREID_BUILD_DIR=str(directory), NEREID_WORKSPACE=str(workspace))
                result = subprocess.run(['bash', str(TOOLS / 'build-hoki.sh')],
                                        env=env, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Local build directory must', result.stderr)

    def test_ambiguous_remote_staging_paths_are_rejected_before_connecting(self):
        for directory in ('/', '/tmp/../source', '/tmp/..', '/tmp/./source', '/tmp/.', '/tmp/space here'):
            with self.subTest(directory=directory):
                env = dict(os.environ, NEREID_BUILD_HOST='unused-builder',
                           NEREID_BUILD_DIR=directory)
                result = subprocess.run(['bash', str(TOOLS / 'build-hoki.sh')],
                                        env=env, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('dedicated absolute build directory', result.stderr)


if __name__ == '__main__':
    unittest.main()
