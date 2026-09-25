#!/usr/bin/env python3
"""Build a fresh userdata filesystem containing a generic rootfs and shared identity.

The seed is a freshly personalized image used only for initial provisioning.
No mounted watch filesystem or existing output is modified.
"""
import argparse
import importlib.util
import json
import os
import re
import stat
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


personalize = load('personalize', ROOT / 'tools/personalize-image.py')
manager = load('rootfs_manager', ROOT / 'recipes-core/hoki-rootfs/files/hoki-rootfs.py')


def provision(bundle, seed_path, output):
    if output.exists() or output.is_symlink():
        raise ValueError('Refusing to overwrite existing output')
    data = manager.manifest(bundle)
    for key, name in [('rootfs', 'rootfs.ext4'), ('recovery', 'recovery.img')]:
        manager.regular(bundle / name)
        if (bundle / name).stat().st_size != data[key + '_size'] or manager.digest(bundle / name) != data[key + '_sha256']:
            raise ValueError('Bundle digest/size mismatch')
    # Do not seed from the generic root itself: require personalizer output with
    # host identity and root SSH access, and check its ext4 consistency.
    for path in (seed_path, bundle / 'rootfs.ext4'):
        subprocess.run(['e2fsck', '-fn', str(path)], check=True, capture_output=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.rootfs-store-', dir=output.parent) as tmp:
        scratch = Path(tmp)
        seed = personalize.Image(seed_path, scratch)
        tree = scratch / 'tree'
        tree.mkdir(mode=0o755)
        store = tree / '.hoki'
        store.mkdir(mode=0o700)
        for name in ('versions', 'incoming', 'state'):
            (store / name).mkdir(mode=0o700)
        destination = store / 'versions' / data['version']
        destination.mkdir(mode=0o700)
        for name in ('manifest.json', 'rootfs.ext4', 'recovery.img'):
            subprocess.run(['cp', '--reflink=auto', '--sparse=always', str(bundle / name), str(destination / name)], check=True)
            (destination / name).chmod(0o600)
        (destination / 'recovery.sha256').write_text(data['recovery_sha256'] + '\n')
        (destination / 'recovery.size').write_text(str(data['recovery_size']) + '\n')
        (store / 'selection').write_text(data['version'] + ' -\n')
        extracted = scratch / 'extracted'
        extracted.mkdir(mode=0o700)
        for name, source in manager.STATE_PATHS:
            target = store / 'state' / name
            if seed.stat('/' + source) == 'directory':
                dumped = subprocess.run(['debugfs', '-R', 'rdump ' + personalize.quote('/' + source) + ' ' + personalize.quote(extracted), str(seed_path)], capture_output=True)
                diagnostics = dumped.stderr.decode().splitlines()
                if diagnostics and diagnostics[0].startswith('debugfs '):
                    diagnostics.pop(0)
                # Unprivileged extraction cannot chown. Restore exact inode
                # ownership/modes inside the output image below, not on host.
                if dumped.returncode or any(not re.fullmatch(r'(?:dump_file|rdump): Operation not permitted while changing ownership of .*', line) for line in diagnostics):
                    raise ValueError('Seed directory extraction failed')
                shutil.move(str(extracted / Path(source).name), str(target))
            else:
                target.mkdir(mode=0o700)
        for user in ('root', 'ceres'):
            (store / 'state/home' / user).mkdir(mode=0o700, exist_ok=True)
        if not (store / 'state/home/root/.ssh/authorized_keys').is_file():
            raise ValueError('Seed lacks root SSH authorized keys')
        identity = store / 'state/identity'
        identity.mkdir(mode=0o700)
        for name, source in [('ssh_host_ecdsa_key', '/etc/ssh/ssh_host_ecdsa_key'),
                             ('ssh_host_ecdsa_key.pub', '/etc/ssh/ssh_host_ecdsa_key.pub'),
                             ('localtime', '/etc/localtime'), ('timezone', '/etc/timezone')]:
            if name == 'localtime' and seed.stat(source) == 'symlink':
                zone = seed.read('/etc/timezone').decode().strip()
                if not re.fullmatch(r'[A-Za-z0-9_+-]+(?:/[A-Za-z0-9_+-]+)*', zone):
                    raise ValueError('Invalid seed timezone')
                source = '/usr/share/zoneinfo/' + zone
            payload = seed.read(source)
            if not payload:
                raise ValueError('Seed lacks required provisioning file')
            (identity / name).write_bytes(payload)
            (identity / name).chmod(0o600 if name == 'ssh_host_ecdsa_key' else 0o644)
        (identity / 'machine-id').write_text(os.urandom(16).hex() + '\n')
        (identity / 'machine-id').chmod(0o444)
        # A 4 GiB filesystem fits the observed 4.4 GiB userdata partition. This
        # changes its contents only when deliberately flashed, not its layout.
        result = scratch / 'userdata.ext4'
        with result.open('xb') as f:
            f.truncate(4 * 1024**3)
        result.chmod(0o600)
        subprocess.run(['mkfs.ext4', '-q', '-F', '-L', 'hoki_userdata', '-O',
                        'none,has_journal,ext_attr,resize_inode,dir_index,filetype,extent,64bit,flex_bg,sparse_super,large_file,huge_file,dir_nlink,extra_isize,metadata_csum',
                        '-d', str(tree), str(result)], check=True)
        image = personalize.Image(result, scratch)
        # mkfs -d inherits workstation ownership. Restore the seed's exact
        # owners and modes for shared directories; other store files are root.
        commands = []
        for path in [tree] + sorted(tree.rglob('*')):
            relative = '/' + path.relative_to(tree).as_posix()
            if relative == '/.':
                relative = '/'
            uid = gid = 0
            for name, source in manager.STATE_PATHS:
                prefix = '/.hoki/state/' + name
                if relative == prefix or relative.startswith(prefix + '/'):
                    original = '/' + source + relative[len(prefix):]
                    if seed.stat(original) is not None:
                        inode = seed.command('stat ' + personalize.quote(original)).decode()
                        owners = re.search(r'User:\s+(\d+)\s+Group:\s+(\d+)', inode)
                        permissions = re.search(r'Mode:\s+([0-7]+)', inode)
                        if not owners or not permissions:
                            raise ValueError('Cannot read seed inode metadata')
                        uid, gid = map(int, owners.groups())
                        mode = stat.S_IFMT(path.lstat().st_mode) | int(permissions.group(1), 8)
                        commands.append('set_inode_field ' + personalize.quote(relative) + ' mode 0' + format(mode, 'o'))
                    break
            commands += ['set_inode_field ' + personalize.quote(relative) + ' uid ' + str(uid),
                         'set_inode_field ' + personalize.quote(relative) + ' gid ' + str(gid)]
        batch = scratch / 'ownership.debugfs'
        batch.write_text('\n'.join(commands) + '\n')
        completed = subprocess.run(['debugfs', '-w', '-f', str(batch), str(result)], capture_output=True)
        errors = completed.stderr.decode().splitlines()
        if errors and errors[0].startswith('debugfs '):
            errors.pop(0)
        if completed.returncode or errors:
            raise ValueError('Failed to normalize image ownership')
        # Read back required metadata and identities without printing secrets.
        assert image.read('/.hoki/selection') == (data['version'] + ' -\n').encode()
        assert image.read('/.hoki/state/identity/ssh_host_ecdsa_key') == seed.read('/etc/ssh/ssh_host_ecdsa_key')
        assert image.read('/.hoki/state/home/root/.ssh/authorized_keys')
        assert image.read('/.hoki/versions/' + data['version'] + '/manifest.json') == (bundle / 'manifest.json').read_bytes()
        subprocess.run(['e2fsck', '-fn', str(result)], check=True, capture_output=True)
        with result.open('rb') as f:
            os.fsync(f.fileno())
        # Atomic publication without overwriting an output created concurrently.
        os.link(result, output)
        manager.sync_dir(output.parent)
    return {'version': data['version'], 'bytes': output.stat().st_size,
            'sha256': manager.digest(output), 'result': 'PASS'}


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('bundle', type=Path)
    p.add_argument('seed', type=Path)
    p.add_argument('output', type=Path)
    a = p.parse_args()
    print(json.dumps(provision(a.bundle, a.seed, a.output), indent=2))
