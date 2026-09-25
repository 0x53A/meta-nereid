#!/usr/bin/env python3
"""Validate the current Android boot artifact and its embedded rootfs selector."""
import argparse
import gzip
import hashlib
from pathlib import Path
import struct


def cpio_files(data):
    pos = 0
    result = {}
    while pos + 110 <= len(data):
        header = data[pos:pos + 110]
        if header[:6] not in (b'070701', b'070702'):
            raise ValueError('Unsupported initramfs archive')
        size = int(header[54:62], 16)
        namesize = int(header[94:102], 16)
        if namesize < 1 or pos + 110 + namesize > len(data):
            raise ValueError('Truncated archive name')
        name = data[pos + 110:pos + 110 + namesize - 1].decode()
        pos = (pos + 110 + namesize + 3) & ~3
        if name == 'TRAILER!!!':
            return result
        if pos + size > len(data):
            raise ValueError('Truncated archive payload')
        result[name.removeprefix('./')] = data[pos:pos + size]
        pos = (pos + size + 3) & ~3
    raise ValueError('Missing archive trailer')


def check(boot, initramfs, source_dir):
    image = boot.read_bytes()
    if image[:8] != b'ANDROID!' or not 48 <= len(image) <= 32 * 1024 * 1024:
        raise ValueError('Invalid recovery image')
    kernel_size = struct.unpack_from('<I', image, 8)[0]
    ramdisk_size = struct.unpack_from('<I', image, 16)[0]
    page = struct.unpack_from('<I', image, 36)[0]
    if page < 512 or page > 65536 or page & (page - 1) or kernel_size == 0:
        raise ValueError('Invalid Android boot layout')
    start = page + ((kernel_size + page - 1) // page) * page
    ramdisk = image[start:start + ramdisk_size]
    if len(ramdisk) != ramdisk_size or ramdisk != initramfs.read_bytes():
        raise ValueError('Boot artifact contains a different initramfs')
    files = cpio_files(gzip.decompress(ramdisk))
    if b'hoki_select_root' not in files.get('init', b''):
        raise ValueError('Managed-root hook missing from init')
    for member, source in [('hoki-rootfs-init.sh', 'hoki-rootfs-init.sh'), ('hoki-state-paths', 'state-paths')]:
        if files.get(member) != (source_dir / source).read_bytes():
            raise ValueError('Missing/stale initramfs file: ' + member)
    return {'bytes': len(image), 'sha256': hashlib.sha256(image).hexdigest(),
            'ramdisk_bytes': ramdisk_size, 'result': 'PASS'}


if __name__ == '__main__':
    import json
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('boot', type=Path)
    p.add_argument('initramfs', type=Path)
    a = p.parse_args()
    source = Path(__file__).resolve().parents[1] / 'recipes-core/hoki-rootfs/files'
    print(json.dumps(check(a.boot, a.initramfs, source), indent=2))
