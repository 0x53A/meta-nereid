#!/usr/bin/env python3
"""Fingerprint the inputs to the independently built Bluetooth SSH payload."""
import hashlib
from pathlib import Path

root = Path(__file__).resolve().parent.parent
project = root / 'ble-ssh/watch-rs'
files = [project / name for name in ('Cargo.toml', 'Cargo.lock', 'shell.nix',
         'ble-ssh-watch.service', 'ble-ssh-watch.env', 'com.ble_ssh.conf')]
for directory in ('src', '.cargo'):
    files.extend(p for p in (project / directory).rglob('*')
                 if p.is_file() and 'target' not in p.relative_to(project).parts)
files.extend(root / 'meta-nereid' / name for name in
             ('build-ble-ssh.sh', 'host-linker.sh', 'ble-ssh-fingerprint.py'))
files.extend(p for p in (root / 'ble-ssh/shared').rglob('*') if p.is_file())
files.extend(p for p in (root / 'ble-ssh/third-party').rglob('*')
             if p.is_file() and not {'target', '.git'} & set(p.relative_to(root / 'ble-ssh/third-party').parts))
digest = hashlib.sha256()
for path in sorted(files):
    digest.update(str(path.relative_to(root)).encode() + b'\0')
    digest.update(path.read_bytes())
print(digest.hexdigest())
