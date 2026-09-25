#!/usr/bin/env python3
"""Create a generic Hoki version bundle for upload to userdata/.hoki/incoming."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


def sha256(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('version')
    p.add_argument('rootfs', type=Path)
    p.add_argument('recovery', type=Path)
    p.add_argument('output', type=Path, help='New directory, named after the version')
    a = p.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_-]{0,63}', a.version) or a.version == 'legacy':
        p.error('Invalid version identifier')
    if a.output.name != a.version:
        p.error('Output directory must be named after the version')
    if not 0 < a.recovery.stat().st_size <= 32 * 1024 * 1024:
        p.error('Invalid recovery image size')
    with a.recovery.open('rb') as f:
        if f.read(8) != b'ANDROID!':
            p.error('Expected Android boot-format recovery image')
    subprocess.run(['e2fsck', '-fn', str(a.rootfs)], check=True)
    a.output.mkdir(mode=0o700)
    data = {'format': 1, 'version': a.version}
    for name, source, target in (('rootfs', a.rootfs, 'rootfs.ext4'),
                                  ('recovery', a.recovery, 'recovery.img')):
        destination = a.output / target
        subprocess.run(['cp', '--reflink=auto', '--sparse=always', str(source), str(destination)], check=True)
        destination.chmod(0o600)
        data[name + '_sha256'] = sha256(destination)
        data[name + '_size'] = destination.stat().st_size
    (a.output / 'manifest.json').write_text(json.dumps(data, indent=2) + '\n')
    print(a.output)


if __name__ == '__main__':
    main()
