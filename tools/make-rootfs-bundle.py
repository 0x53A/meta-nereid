#!/usr/bin/env python3
"""Create a generic Hoki version bundle for upload to userdata/.hoki/incoming."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tarfile


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
    p.add_argument('--sbom', type=Path, help='Image SPDX; defaults to the file beside the rootfs')
    p.add_argument('--cargo-sbom', type=Path, help='Cargo report directory; defaults to cargo-sbom beside the rootfs')
    a = p.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_-]{0,63}', a.version) or a.version == 'legacy':
        p.error('Invalid version identifier')
    if a.output.name != a.version:
        p.error('Output directory must be named after the version')
    rootfs = a.rootfs.resolve(strict=True)
    sbom = a.sbom or rootfs.with_name(rootfs.name.removesuffix('.ext4') + '.spdx.json')
    image_manifest = rootfs.with_name(rootfs.name.removesuffix('.ext4') + '.manifest')
    if not sbom.is_file() or not image_manifest.is_file():
        p.error('The matching image SPDX and package manifest are required beside the rootfs')
    if not 0 < a.recovery.stat().st_size <= 32 * 1024 * 1024:
        p.error('Invalid recovery image size')
    with a.recovery.open('rb') as f:
        if f.read(8) != b'ANDROID!':
            p.error('Expected Android boot-format recovery image')
    subprocess.run(['e2fsck', '-fn', str(rootfs)], check=True)
    a.output.mkdir(mode=0o700)
    data = {'format': 1, 'version': a.version}
    for name, source, target in (('rootfs', rootfs, 'rootfs.ext4'),
                                  ('recovery', a.recovery, 'recovery.img')):
        destination = a.output / target
        subprocess.run(['cp', '--reflink=auto', '--sparse=always', str(source), str(destination)], check=True)
        destination.chmod(0o600)
        data[name + '_sha256'] = sha256(destination)
        data[name + '_size'] = destination.stat().st_size
    if sbom.is_file():
        cargo_dir = a.cargo_sbom or rootfs.parent / 'cargo-sbom'
        sbom_destination = a.output / 'sbom.spdx.json'
        subprocess.run(['cp', '--reflink=auto', str(sbom), str(sbom_destination)], check=True)
        sbom_destination.chmod(0o600)
        subprocess.run([sys.executable, str(Path(__file__).with_name('sbom-license-index.py')),
                        str(sbom), str(image_manifest), str(a.output / 'licenses.tsv'),
                        '--cargo-dir', str(cargo_dir)], check=True)
        if cargo_dir.is_dir():
            with tarfile.open(a.output / 'cargo-sbom.tar.gz', 'w:gz') as archive:
                archive.add(cargo_dir, arcname='cargo-sbom')
        for name in ('sbom.spdx.json', 'licenses.tsv', 'cargo-sbom.tar.gz'):
            path = a.output / name
            if path.exists():
                path.chmod(0o600)
                data[name + '_sha256'] = sha256(path)
                data[name + '_size'] = path.stat().st_size
    (a.output / 'manifest.json').write_text(json.dumps(data, indent=2) + '\n')
    print(a.output)


if __name__ == '__main__':
    main()
