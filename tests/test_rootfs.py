import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
FILES = ROOT / 'meta-nereid/recipes-core/hoki-rootfs/files'
spec = importlib.util.spec_from_file_location('manager', FILES / 'hoki-rootfs.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class RootfsTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.store = self.base / 'store'
        self.store.mkdir()
        for name in ('versions', 'incoming', 'state'):
            (self.store / name).mkdir()
        m.atomic(self.store / 'selection', 'legacy -\n')
        self.recovery = self.base / 'recovery'
        self.recovery.write_bytes(b'ANDROID!' + b'x' * 4096)

    def bundle(self, name='v1'):
        p = self.store / 'incoming' / name
        p.mkdir()
        image = p / 'rootfs.ext4'
        with image.open('wb') as f:
            f.truncate(8 * 1024 * 1024)
        subprocess.run(['mkfs.ext4', '-q', '-F', str(image)], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        (p / 'recovery.img').write_bytes(self.recovery.read_bytes())
        data = {'format': 1, 'version': name}
        for key, path in [('rootfs', image), ('recovery', p / 'recovery.img')]:
            data[key + '_sha256'] = m.digest(path)
            data[key + '_size'] = path.stat().st_size
        (p / 'manifest.json').write_text(json.dumps(data))
        return p

    def test_stage_activate_confirm(self):
        p = self.bundle()
        inode = (p / 'rootfs.ext4').stat().st_ino
        m.stage(self.store, p)
        self.assertEqual(inode, (self.store / 'versions/v1/rootfs.ext4').stat().st_ino)
        m.activate(self.store, 'v1', self.recovery)
        self.assertEqual(m.selection(self.store), ('legacy', 'v1'))
        # Boot consumes the trial; confirmation promotes the actual booted ID.
        m.atomic(self.store / 'selection', 'legacy -\n')
        booted = self.base / 'booted'
        booted.write_text('v1\n')
        m.confirm(self.store, booted, self.recovery)
        self.assertEqual(m.selection(self.store), ('v1', '-'))

    def test_hash_failure_never_publishes(self):
        p = self.bundle()
        (p / 'rootfs.ext4').write_bytes(b'corrupt')
        with self.assertRaises(ValueError):
            m.stage(self.store, p)
        self.assertFalse((self.store / 'versions/v1').exists())
        self.assertEqual(m.selection(self.store), ('legacy', '-'))

    def test_interrupted_staging_metadata_is_retryable(self):
        p = self.bundle()
        (p / 'recovery.sha256').write_text('interrupted staging')
        m.stage(self.store, p)
        self.assertEqual((self.store / 'versions/v1/recovery.sha256').read_text(), m.digest(self.recovery) + '\n')

    def test_recovery_mismatch_preserves_selection(self):
        m.stage(self.store, self.bundle())
        self.recovery.write_bytes(b'y' * 5000)
        with self.assertRaises(ValueError):
            m.activate(self.store, 'v1', self.recovery)
        self.assertEqual(m.selection(self.store), ('legacy', '-'))

    def test_no_overwrite_existing_version(self):
        m.stage(self.store, self.bundle())
        with self.assertRaises(ValueError):
            m.stage(self.store, self.bundle())

    def test_reject_symlink_payload(self):
        p = self.bundle()
        (p / 'recovery.img').unlink()
        (p / 'recovery.img').symlink_to(self.recovery)
        with self.assertRaises(ValueError):
            m.stage(self.store, p)

    def test_failed_atomic_replace_keeps_old_selection(self):
        with patch.object(m.os, 'replace', side_effect=OSError('simulated power boundary')):
            with self.assertRaises(OSError):
                m.atomic(self.store / 'selection', 'new -\n')
        self.assertEqual(m.selection(self.store), ('legacy', '-'))
        self.assertEqual(list(self.store.glob('.write-*')), [])

    def test_pending_trial_not_silently_replaced(self):
        m.stage(self.store, self.bundle())
        m.atomic(self.store / 'selection', 'legacy other\n')
        with self.assertRaises(ValueError):
            m.activate(self.store, 'v1', self.recovery)
        self.assertEqual(m.selection(self.store), ('legacy', 'other'))

    def test_reject_bad_filesystem_even_with_matching_hash(self):
        p = self.bundle()
        (p / 'rootfs.ext4').write_bytes(b'not ext4')
        data = json.loads((p / 'manifest.json').read_text())
        data['rootfs_size'] = 8
        data['rootfs_sha256'] = m.digest(p / 'rootfs.ext4')
        (p / 'manifest.json').write_text(json.dumps(data))
        with self.assertRaises(subprocess.CalledProcessError):
            m.stage(self.store, p)
        self.assertFalse((self.store / 'versions/v1').exists())

    def test_selection_parser_agrees_with_shell(self):
        values = [('legacy -\n', True), ('v1 next\n', True), ('../../bad -\n', False),
                  ('v1 -\nextra\n', False), ('v1\n', False), ('v1 x extra\n', False)]
        for text, valid in values:
            with self.subTest(text=text):
                (self.store / 'selection').write_text(text)
                command = '. "$1"; hoki_read_selection "$2"'
                result = subprocess.run(['sh', '-c', command, 'test', str(FILES / 'hoki-rootfs-init.sh'), str(self.store)])
                self.assertEqual(result.returncode == 0, valid)
                if valid:
                    m.selection(self.store)
                else:
                    with self.assertRaises(ValueError):
                        m.selection(self.store)

    def test_boot_consumes_trial_before_mount_and_falls_back(self):
        m.atomic(self.store / 'selection', 'good trial\n')
        script = """. "$1"
        hoki_mount_version() {
            [ "$(cat "$2/selection")" = 'good -' ] || return 2
        }
        # An explicit stub observes selection at the instant boot would mount.
        store="$2"
        hoki_mount_version() {
            [ "$(cat "$store/selection")" = 'good -' ] || return 2
            echo "$1" >> "$store/attempts"
            [ "$1" = good ]
        }
        hoki_select_root "$store"
        """
        subprocess.run(['sh', '-c', script, 'test', str(FILES / 'hoki-rootfs-init.sh'), str(self.store)], check=True)
        self.assertEqual((self.store / 'attempts').read_text(), 'trial\ngood\n')
        self.assertEqual(m.selection(self.store), ('good', '-'))

    def test_boot_partial_mount_failure_stops_without_fallback(self):
        m.atomic(self.store / 'selection', 'good trial\n')
        script = '. "$1"; hoki_mount_version() { return 2; }; hoki_select_root "$2"'
        result = subprocess.run(['sh', '-c', script, 'test', str(FILES / 'hoki-rootfs-init.sh'), str(self.store)])
        self.assertEqual(result.returncode, 2)
        self.assertEqual(m.selection(self.store), ('good', '-'))

    def test_patch_preserves_legacy_path_and_rejects_drift(self):
        original = ROOT / 'meta-asteroid/recipes-core/initrdscripts/initramfs-scripts-android/init.sh'
        p = self.base / 'init'
        p.write_bytes(original.read_bytes())
        patcher = ROOT / 'meta-nereid/recipes-core/initrdscripts/files/hoki-rootfs-hook.py'
        subprocess.run(['python3', str(patcher), str(p)], check=True)
        subprocess.run(['sh', '-n', str(p)], check=True)
        self.assertIn('asteroidos.ext4', p.read_text())
        self.assertIn('hoki_select_root', p.read_text())
        result = subprocess.run(['python3', str(patcher), str(p)], stderr=subprocess.PIPE)
        # Applying twice must not silently nest the managed boot block.
        self.assertNotEqual(result.returncode, 0)


if __name__ == '__main__':
    unittest.main()
