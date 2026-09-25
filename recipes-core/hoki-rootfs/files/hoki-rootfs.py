#!/usr/bin/env python3
"""Manage Hoki rootfs versions. Never writes recovery or reboots implicitly."""
import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile

STORE = Path('/userdata/.hoki')
RECOVERY = Path('/dev/mmcblk0p29')
STATE_PATHS = (('home', 'home'), ('bluetooth', 'var/lib/bluetooth'),
               ('connman', 'var/lib/connman'), ('tailscale', 'var/lib/tailscale'),
               )


def version(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_-]{0,63}', value) or value == 'legacy':
        raise ValueError('Invalid version identifier')
    return value


def regular(path):
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError('Expected regular file: ' + str(path))


def digest(path, size=None):
    h = hashlib.sha256()
    with path.open('rb') as f:
        remaining = size
        while remaining is None or remaining > 0:
            chunk = f.read(1024 * 1024 if remaining is None else min(remaining, 1024 * 1024))
            if not chunk:
                if remaining:
                    raise ValueError('Truncated file/device: ' + str(path))
                break
            h.update(chunk)
            if remaining is not None:
                remaining -= len(chunk)
    return h.hexdigest()


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic(path, text):
    fd, name = tempfile.mkstemp(prefix='.write-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        os.replace(name, path)
        sync_dir(path.parent)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def selection(store):
    regular(store / 'selection')
    text = (store / 'selection').read_text()
    fields = text.rstrip('\n').split(' ')
    if len(fields) != 2 or text != ' '.join(fields) + '\n':
        raise ValueError('Malformed boot selection')
    good, trial = fields
    if good != 'legacy':
        version(good)
    if trial != '-':
        version(trial)
    return good, trial


@contextlib.contextmanager
def locked(store):
    if store.is_symlink() or not store.is_dir():
        raise ValueError('Missing or unsafe version store')
    fd = os.open(store / 'lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)


def manifest(directory):
    regular(directory / 'manifest.json')
    data = json.loads((directory / 'manifest.json').read_text())
    keys = {'format', 'version', 'rootfs_sha256', 'rootfs_size', 'recovery_sha256', 'recovery_size'}
    if not isinstance(data, dict) or set(data) != keys or type(data['format']) is not int or data['format'] != 1:
        raise ValueError('Unsupported bundle manifest')
    version(data['version'])
    for name in ('rootfs', 'recovery'):
        if not isinstance(data[name + '_sha256'], str) or not re.fullmatch('[0-9a-f]{64}', data[name + '_sha256']):
            raise ValueError('Invalid digest')
        size = data[name + '_size']
        if type(size) is not int or size <= 0:
            raise ValueError('Invalid size')
    if data['recovery_size'] > 32 * 1024 * 1024:
        raise ValueError('Recovery image exceeds partition')
    return data


def compatible(data, recovery):
    if digest(recovery, data['recovery_size']) != data['recovery_sha256']:
        raise ValueError('Recovery image differs: install matching recovery separately before activation')


def stage(store, incoming):
    # Upload directly into incoming/ so publication is a rename, not a second
    # rootfs copy. Incoming transfers have no effect on boot selection.
    if incoming.is_symlink() or incoming.parent.resolve() != (store / 'incoming').resolve():
        raise ValueError('Bundle must be a real directory immediately under store/incoming')
    data = manifest(incoming)
    if incoming.name != data['version']:
        raise ValueError('Bundle directory and version differ')
    destination = store / 'versions' / data['version']
    if destination.exists():
        raise ValueError('Version already exists; never overwrite a bootable image')
    required = {'manifest.json', 'rootfs.ext4', 'recovery.img'}
    allowed = required | {'recovery.sha256', 'recovery.size'}
    present = {p.name for p in incoming.iterdir()}
    if not required <= present or not present <= allowed:
        raise ValueError('Unexpected bundle contents')
    for name in present - required:
        regular(incoming / name)
    for name, filename in (('rootfs', 'rootfs.ext4'), ('recovery', 'recovery.img')):
        path = incoming / filename
        regular(path)
        if path.stat().st_size != data[name + '_size'] or digest(path) != data[name + '_sha256']:
            raise ValueError('Bundle verification failed: ' + filename)
    # Require a clean image; noload is used by the boot-time read-only mount.
    subprocess.run(['e2fsck', '-fn', str(incoming / 'rootfs.ext4')], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    for name in ('recovery.sha256', 'recovery.size'):
        key = name.replace('.', '_')
        atomic(incoming / name, str(data[key]) + '\n')
    for path in incoming.iterdir():
        path.chmod(0o600)
        with path.open('rb') as f:
            os.fsync(f.fileno())
    incoming.chmod(0o700)
    sync_dir(incoming)
    os.rename(incoming, destination)
    sync_dir(destination.parent)
    sync_dir(store / 'incoming')
    return data['version']


def activate(store, name, recovery):
    name = version(name)
    data = manifest(store / 'versions' / name)
    if data['version'] != name:
        raise ValueError('Version metadata mismatch')
    compatible(data, recovery)
    good, trial = selection(store)
    if trial != '-':
        raise ValueError('A trial is already pending; cancel it explicitly first')
    if name == good:
        raise ValueError('Version is already confirmed')
    # Keep free headroom for ext4 metadata, overlays, state and journals.
    if shutil.disk_usage(store).free < 128 * 1024 * 1024:
        raise ValueError('Less than 128 MiB free reserve')
    atomic(store / 'selection', good + ' ' + name + '\n')


def confirm(store, booted, recovery):
    name = version(booted.read_text().strip())
    data = manifest(store / 'versions' / name)
    compatible(data, recovery)
    good, trial = selection(store)
    if trial != '-':
        raise ValueError('Cannot confirm while another trial is pending')
    atomic(store / 'selection', name + ' -\n')


def initialize(store, seed):
    if store.exists():
        raise ValueError('Version store already exists')
    # The seed is a fresh, provisioned tree, not a copy of old application data.
    # Run as root so SSH/ceres ownership is preserved by cp -a.
    store.mkdir(mode=0o700)
    try:
        for name in ('versions', 'incoming', 'state'):
            (store / name).mkdir(mode=0o700)
        for name, target in STATE_PATHS:
            destination = store / 'state' / name
            source = seed / target
            if source.is_dir() and not source.is_symlink():
                subprocess.run(['cp', '-a', str(source), str(destination)], check=True)
            else:
                destination.mkdir(mode=0o700)
        identity = store / 'state/identity'
        identity.mkdir(mode=0o700)
        for name, source in [('ssh_host_ecdsa_key', seed / 'etc/ssh/ssh_host_ecdsa_key'),
                             ('ssh_host_ecdsa_key.pub', seed / 'etc/ssh/ssh_host_ecdsa_key.pub'),
                             ('localtime', seed / 'etc/localtime'),
                             ('timezone', seed / 'etc/timezone')]:
            if not source.is_file():
                raise ValueError('Missing initial provisioning: ' + str(source))
            shutil.copyfile(source, identity / name)
            (identity / name).chmod(0o600 if name == 'ssh_host_ecdsa_key' else 0o644)
        (identity / 'machine-id').write_text(os.urandom(16).hex() + '\n')
        (identity / 'machine-id').chmod(0o444)
        # This store starts with legacy as its confirmed rescue path. First
        # managed boot is a trial until explicitly confirmed after validation.
        os.sync()
        atomic(store / 'selection', 'legacy -\n')
        sync_dir(store.parent)
    except Exception:
        # Retain partial state for inspection; never recursively delete a
        # path provided by the caller after a failed provisioning operation.
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--store', type=Path, default=STORE)
    commands = parser.add_subparsers(dest='command', required=True)
    init = commands.add_parser('initialize')
    init.add_argument('--seed', type=Path, required=True)
    upload = commands.add_parser('stage')
    upload.add_argument('incoming', type=Path)
    activate_parser = commands.add_parser('activate')
    activate_parser.add_argument('version')
    for name in ('status', 'cancel', 'confirm'):
        commands.add_parser(name)
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error('Run on the watch as root')
    if args.command == 'initialize':
        initialize(args.store, args.seed)
        return
    with locked(args.store):
        if args.command == 'stage':
            print(stage(args.store, args.incoming))
        elif args.command == 'activate':
            activate(args.store, args.version, RECOVERY)
            print('Trial selected. Reboot normally when ready.')
        elif args.command == 'confirm':
            confirm(args.store, Path('/etc/hoki-rootfs-booted'), RECOVERY)
            print('Current version confirmed. Previous versions retained.')
        elif args.command == 'cancel':
            good, _ = selection(args.store)
            atomic(args.store / 'selection', good + ' -\n')
        else:
            good, trial = selection(args.store)
            print(json.dumps({'confirmed': good, 'trial': trial}))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as exc:
        raise SystemExit(str(exc))
