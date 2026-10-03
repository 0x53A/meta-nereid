"""Exercise real compressed data validation and migration compatibility."""
import json
from pathlib import Path
import subprocess
from test_rootfs import RootfsTests, m


class SquashfsTests(RootfsTests):
    def compressed(self):
        p = self.bundle()
        tree = self.base / 'tree'
        tree.mkdir()
        (tree / 'payload').write_bytes(bytes(range(256)) * 2048)
        (tree / 'link').symlink_to('payload')
        image = p / 'rootfs.squashfs'
        subprocess.run(['mksquashfs', str(tree), str(image), '-comp', 'lz4',
                        '-b', '131072', '-noappend', '-no-progress', '-processors', '2'],
                       check=True, stdout=subprocess.DEVNULL)
        (p / 'rootfs.ext4').unlink()
        data = json.loads((p / 'manifest.json').read_text())
        data.update(format=2, rootfs_type='squashfs', rootfs_file='rootfs.squashfs',
                    rootfs_size=image.stat().st_size, rootfs_sha256=m.digest(image))
        (p / 'manifest.json').write_text(json.dumps(data))
        return p

    def test_compressed_stage_and_activate(self):
        p = self.compressed()
        m.stage(self.store, p)
        m.activate(self.store, 'v1', self.recovery)
        self.assertEqual(m.selection(self.store), ('legacy', 'v1'))

    def test_corrupt_compressed_data_with_matching_digest(self):
        p = self.compressed()
        image = p / 'rootfs.squashfs'
        data = bytearray(image.read_bytes())
        # First data block starts after the 96-byte superblock.
        data[96:200] = b'\xff' * 104
        image.write_bytes(data)
        manifest = json.loads((p / 'manifest.json').read_text())
        manifest['rootfs_sha256'] = m.digest(image)
        (p / 'manifest.json').write_text(json.dumps(manifest))
        with self.assertRaises(subprocess.CalledProcessError):
            m.stage(self.store, p)
        self.assertFalse((self.store / 'versions/v1').exists())

    def test_type_filename_rejected(self):
        p = self.compressed()
        data = json.loads((p / 'manifest.json').read_text())
        for kind, filename in [('btrfs', 'rootfs.btrfs'), ('squashfs', '../payload'),
                               ('squashfs', 'rootfs.ext4')]:
            data.update(rootfs_type=kind, rootfs_file=filename)
            (p / 'manifest.json').write_text(json.dumps(data))
            with self.assertRaises(ValueError):
                m.manifest(p)

    def test_extra_ext4_rejected(self):
        p = self.compressed()
        (p / 'rootfs.ext4').write_bytes(b'ambiguous')
        with self.assertRaises(ValueError):
            m.stage(self.store, p)

    def boot_mount_attempt(self, directory):
        source = Path(m.__file__).with_name('hoki-rootfs-init.sh').read_text()
        source = source.replace('store=/sdcard/.hoki', 'store="$TEST_STORE"')
        script = self.base / 'init-test.sh'
        script.write_text(source)
        attempts = self.base / 'mount-attempt'
        command = '''. "$1"
        TEST_STORE="$2"; TEST_RECOVERY="$3"; ATTEMPTS="$4"
        dd() { cat "$TEST_RECOVERY"; }
        mkdir() { return 0; }
        mount() { printf '%s\\n' "$*" > "$ATTEMPTS"; return 1; }
        hoki_mount_version v1
        '''
        result = subprocess.run(['sh', '-c', command, 'test', str(script), str(self.store),
                                 str(self.recovery), str(attempts)])
        self.assertEqual(result.returncode, 1)
        return attempts.read_text() if attempts.exists() else None

    def test_boot_selects_squashfs_mount(self):
        m.stage(self.store, self.compressed())
        attempt = self.boot_mount_attempt(self.store / 'versions/v1')
        self.assertIn('-t squashfs -o ro,loop ', attempt)
        self.assertIn('/rootfs.squashfs /hoki-lower', attempt)

    def test_boot_retains_ext4_noload(self):
        m.stage(self.store, self.bundle())
        attempt = self.boot_mount_attempt(self.store / 'versions/v1')
        self.assertIn('-t ext4 -o ro,noload,loop ', attempt)

    def test_boot_rejects_ambiguous_payloads(self):
        m.stage(self.store, self.compressed())
        directory = self.store / 'versions/v1'
        (directory / 'rootfs.ext4').write_bytes(b'ambiguous')
        self.assertIsNone(self.boot_mount_attempt(directory))
