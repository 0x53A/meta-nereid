"""Build input integrity checks for source-built runtime recipes."""
import importlib.util
from pathlib import Path
import tempfile
import tomllib
import unittest

TOOLS = Path(__file__).resolve().parents[1]


def load(name):
    spec = importlib.util.spec_from_file_location(name, TOOLS / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class RuntimeSources(unittest.TestCase):
    def test_locked_metadata_is_current(self):
        generator = load('update-runtime-recipes')
        for name, content in generator.outputs().items():
            self.assertEqual((generator.ROOT / name).read_text(), content, name)

    def test_staging_is_self_contained_and_complete(self):
        stager = load('stage-project-sources')
        with tempfile.TemporaryDirectory() as tmp:
            stager.stage(tmp)
            root = Path(tmp)
            self.assertTrue((root / 'Cargo.toml').is_file())
            self.assertTrue((root / 'Cargo.lock').is_file())
            self.assertTrue((root / 'shared/sleep_client.rs').is_file())
            self.assertTrue((root / 'hoki-gps-recorder/geocluerecorder.cpp').is_file())
            self.assertTrue((root / 'hoki-activity/Cargo.toml').is_file())
            self.assertTrue((root / 'hoki-activity/ui/main.slint').is_file())
            self.assertTrue((root / 'hoki-activity/daemon.py').is_file())
            self.assertTrue((root / 'hoki-health-recorder/deploy/health_broker.py').is_file())
            self.assertTrue((root / 'hoki-health-recorder/ssc/ssc_worker.h').is_file())
            self.assertTrue((root / 'hoki-wasm-guest/src/lib.rs').is_file())
            self.assertTrue((root / 'ble-ssh/watch-rs/ble-ssh-watch.service').is_file())
            self.assertTrue((root / 'ble-ssh/shared/transfer.rs').is_file())
            for path in root.rglob('*'):
                self.assertFalse(path.is_symlink(), path)
                self.assertFalse(set(path.relative_to(root).parts) & {'target', 'build', 'data', '.git', '.cargo'}, path)
            self.assertFalse((root / 'hoki-wasm-host/guest.wasm').exists())
            self.assertFalse((root / 'hoki-music/cross-lib').exists())

    def test_runtime_workspace_shares_versions_and_keeps_wasm_optional(self):
        root = TOOLS.parent
        projects = root / 'projects'
        workspace = tomllib.loads((projects / 'Cargo.toml').read_text())
        selected = {line.split('|')[0] for line in (root / 'runtime-projects.txt').read_text().splitlines()
                    if line and not line.startswith('#')}
        self.assertEqual(set(workspace['workspace']['members']), selected)
        self.assertEqual(set(workspace['workspace']['default-members']), selected - {'hoki-wasm-host'})
        for name in selected:
            manifest = tomllib.loads((projects / name / 'Cargo.toml').read_text())
            self.assertNotIn('profile', manifest, name)
            self.assertFalse((projects / name / 'Cargo.lock').exists(), name)
            def check_dependencies(table):
                for key, value in table.items():
                    if key in ('dependencies', 'build-dependencies', 'dev-dependencies'):
                        for dependency, spec in value.items():
                            self.assertTrue(spec.get('workspace'), (name, dependency))
                            self.assertNotIn('version', spec, (name, dependency))
                            self.assertIn(dependency, workspace['workspace']['dependencies'])
                    elif isinstance(value, dict):
                        check_dependencies(value)
            check_dependencies(manifest)
        packages = tomllib.loads((projects / 'Cargo.lock').read_text())['package']
        for name in ('slint', 'slint-build', 'slint-macros'):
            self.assertEqual([p['version'] for p in packages if p['name'] == name], ['1.15.1'])

    def test_image_recipes_do_not_consume_local_runtime_archives(self):
        root = TOOLS.parent
        for name in ('recipes-hoki/hoki-ui/hoki-ui_1.0.bb',
                     'recipes-hoki/hoki-health-recorder/hoki-health-recorder_0.1.0.bb',
                     'recipes-connectivity/ble-ssh/ble-ssh-watch_0.1.0.bb'):
            recipe = (root / name).read_text()
            self.assertNotIn('-runtime.tar.gz', recipe)
            self.assertIn('inherit cargo', recipe)
        wrapper = (TOOLS / 'build-hoki.sh').read_text()
        self.assertNotIn('nix-shell', wrapper)
        self.assertIn('stage-project-sources.py', wrapper)


if __name__ == '__main__':
    unittest.main()
