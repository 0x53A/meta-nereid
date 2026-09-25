#!/usr/bin/env python3
"""Guarded, kernel-only recovery replacement for the currently confirmed rootfs.

Run as a systemd service after preserving logs and stopping/finishing
captures. Does not reboot. A power loss during the physical write still requires
external recovery; the rootfs trial mechanism cannot roll back the kernel.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import struct

PARTITION_SIZE = 32 * 1024 * 1024
METADATA = ('recovery.img', 'recovery.size', 'recovery.sha256', 'manifest.json')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def durable(path, data):
    with path.open('xb') as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    path.chmod(0o600)


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic(path, data):
    temp = path.with_name(path.name + '.kernel-update')
    durable(temp, data)
    os.replace(temp, path)
    sync_dir(path.parent)


def parse_image(data):
    if data[:8] != b'ANDROID!' or len(data) < 4096:
        raise ValueError('Not an Android boot image')
    ks, ka, rs, ra, ss, sa, ta, page, dt, res = struct.unpack_from('<10I', data, 8)
    if page != 4096 or ss or dt or res:
        raise ValueError('Only the verified Android v0 kernel+ramdisk layout is supported')
    align = lambda n: (n + page - 1) // page * page
    size = page + align(ks) + align(rs)
    if not ks or not rs or size > len(data) or size > PARTITION_SIZE:
        raise ValueError('Invalid image size')
    kernel = data[page:page + ks]
    ramdisk = data[page + align(ks):page + align(ks) + rs]
    identity = hashlib.sha1()
    for part in (kernel, ramdisk, b''):
        identity.update(part)
        identity.update(struct.pack('<I', len(part)))
    if identity.digest() != data[576:596]:
        raise ValueError('Android boot ID mismatch')
    header = bytearray(data[:page])
    header[8:12] = bytes(4)
    header[576:608] = bytes(32)
    return size, ramdisk, bytes(header)


def replace(store, device, image, version, old_hash, new_hash, *, test_device=False, hook=None, pre_write=None):
    """Locked transaction; test_device permits a regular file only in host tests."""
    if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_-]{0,63}', version):
        raise ValueError('Invalid version')
    if store.is_symlink() or not store.is_dir():
        raise ValueError('Unsafe rootfs store')
    lock = os.open(store/'lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if (store/'selection').read_text() != version + ' -\n':
            raise ValueError('Expected confirmed version without a trial')
        directory = store/'versions'/version
        if directory.is_symlink() or not directory.is_dir():
            raise ValueError('Unsafe version directory')
        old_meta = {}
        for name in METADATA:
            path = directory/name
            if path.is_symlink() or not path.is_file():
                raise ValueError('Missing/unsafe version metadata: ' + name)
            if path.with_name(name + '.kernel-update').exists():
                raise ValueError('Unresolved prior metadata transaction')
            old_meta[name] = path.read_bytes()
        data = json.loads(old_meta['manifest.json'])
        new = image.read_bytes()
        if sha(new) != new_hash:
            raise ValueError('Uploaded image checksum mismatch')
        new_size, new_ramdisk, new_header = parse_image(new)
        if new_size != len(new):
            raise ValueError('Unexpected trailing image bytes')
        fd = os.open(device, os.O_RDWR | os.O_NOFOLLOW)
        try:
            mode = os.fstat(fd).st_mode
            if not stat.S_ISBLK(mode) and not (test_device and stat.S_ISREG(mode)):
                raise ValueError('Recovery target is not a block device')
            if os.lseek(fd, 0, os.SEEK_END) != PARTITION_SIZE:
                raise ValueError('Wrong recovery partition size')
            os.lseek(fd, 0, os.SEEK_SET)
            with os.fdopen(os.dup(fd), 'rb') as reader:
                old = reader.read()
            if len(old) != PARTITION_SIZE or sha(old) != old_hash:
                raise ValueError('Recovery changed since backup')
            old_size, old_ramdisk, old_header = parse_image(old)
            if new_ramdisk != old_ramdisk or new_header != old_header:
                raise ValueError('Kernel-only update must preserve ramdisk and boot parameters')
            if (data['version'] != version or data['recovery_size'] != old_size
                    or data['recovery_sha256'] != sha(old[:old_size])
                    or old_meta['recovery.img'] != old[:old_size]
                    or old_meta['recovery.size'].strip() != str(old_size).encode()
                    or old_meta['recovery.sha256'].strip() != sha(old[:old_size]).encode()):
                raise ValueError('Current recovery and version metadata do not agree')
            rootfs = directory/'rootfs.ext4'
            if rootfs.is_symlink() or not rootfs.is_file() or rootfs.stat().st_size != data['rootfs_size']:
                raise ValueError('Rootfs file/size does not match metadata')
            digest = hashlib.sha256()
            with rootfs.open('rb') as source:
                for chunk in iter(lambda: source.read(1024*1024), b''):
                    digest.update(chunk)
            if digest.hexdigest() != data['rootfs_sha256']:
                raise ValueError('Rootfs checksum mismatch')
            if shutil.disk_usage(store).free < 128*1024*1024 + 2*PARTITION_SIZE:
                raise ValueError('Insufficient space for backup plus reserve')
            updates = store/'kernel-updates'
            updates.mkdir(mode=0o700, exist_ok=True)
            if updates.is_symlink():
                raise ValueError('Unsafe backup directory')
            backup = updates/new_hash[:16]
            backup.mkdir(mode=0o700)  # Refuse accidental repeat or overwrite.
            durable(backup/'recovery-partition.before', old)
            for name, content in old_meta.items():
                durable(backup/(name + '.before'), content)
            durable(backup/'selection.before', (store/'selection').read_bytes())
            durable(backup/'recovery.new', new)
            sync_dir(backup)
            sync_dir(updates)
            updated = dict(data, recovery_size=new_size, recovery_sha256=sha(new))
            new_meta = {'recovery.img': new, 'recovery.size': (str(new_size)+'\n').encode(),
                        'recovery.sha256': (sha(new)+'\n').encode(),
                        'manifest.json': (json.dumps(updated,indent=2)+'\n').encode()}
            expected_partition = new + old[len(new):]
            def write_device(content, inject=False):
                os.lseek(fd, 0, os.SEEK_SET)
                with os.fdopen(os.dup(fd), 'r+b', buffering=0) as writer:
                    view = memoryview(content)
                    while view:
                        written = writer.write(view[:1024*1024])
                        if not written:
                            raise OSError('Short recovery write')
                        view = view[written:]
                        if inject and hook:
                            hook('write-chunk')
                    os.fsync(writer.fileno())
            def read_device():
                os.lseek(fd, 0, os.SEEK_SET)
                with os.fdopen(os.dup(fd), 'rb') as reader:
                    return reader.read()
            result = {'version':version,'old_partition_sha256':sha(old),
                      'new_partition_sha256':sha(expected_partition),
                      'new_image_sha256':sha(new),'new_image_size':new_size,
                      'backup':str(backup),'rebooted':False}
            if pre_write:
                pre_write()
            try:
                write_device(new, inject=True)
                if read_device() != expected_partition:
                    raise OSError('Recovery readback mismatch')
                if hook:
                    hook('after-write')
                for name, content in new_meta.items():
                    atomic(directory/name, content)
                    if hook:
                        hook('metadata-' + name)
                if (store/'selection').read_text() != version + ' -\n':
                    raise ValueError('Selection unexpectedly changed')
                if read_device()[:new_size] != new:
                    raise OSError('Final recovery verification failed')
                durable(backup/'result.json', (json.dumps(result,indent=2)+'\n').encode())
                sync_dir(backup)
            except BaseException:
                # Normal errors are recoverable while this kernel is still alive.
                # Do not claim this covers power loss or a killed transaction.
                try:
                    write_device(old)
                    for name, content in old_meta.items():
                        pending = directory/(name + '.kernel-update')
                        if pending.exists():
                            pending.unlink()
                        atomic(directory/name, content)
                    if read_device() != old:
                        raise OSError('Rollback readback mismatch')
                    if (backup/'result.json').exists():
                        (backup/'result.json').unlink()
                    durable(backup/'rolled-back', b'Old recovery and metadata restored.\n')
                    sync_dir(backup)
                except BaseException as rollback_error:
                    raise OSError('ROLLBACK FAILED: do not reboot; external recovery required') from rollback_error
                raise
            return result
        finally:
            os.close(fd)
    finally:
        os.close(lock)


def no_active_capture():
    protected = {'hoki-health-recorder', 'hoki-suspend-check', 'arecord', 'parec'}
    for process in Path('/proc').iterdir():
        if not process.name.isdigit():
            continue
        try:
            executable = os.readlink(process/'exe').removesuffix(' (deleted)')
        except FileNotFoundError:
            continue
        if Path(executable).name in protected:
            raise ValueError('Active capture/suspend process: coordinate before update')


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--image',required=True,type=Path)
    p.add_argument('--expected-version',required=True)
    p.add_argument('--expected-boot-id',required=True)
    p.add_argument('--old-partition-sha256',required=True)
    p.add_argument('--image-sha256',required=True)
    args=p.parse_args()
    if os.geteuid()!=0:
        p.error('Must run as root')
    if Path('/proc/sys/kernel/random/boot_id').read_text().strip()!=args.expected_boot_id:
        p.error('Watch rebooted since preparation')
    if Path('/etc/hoki-rootfs-booted').read_text().strip()!=args.expected_version:
        p.error('Unexpected booted rootfs')
    device=Path('/dev/disk/by-partlabel/recovery').resolve(strict=True)
    if device!=Path('/dev/mmcblk0p29'):
        p.error('Unexpected Hoki recovery partition')
    if not os.path.ismount('/userdata'):
        p.error('Managed userdata is not mounted')
    no_active_capture()
    result=replace(Path('/userdata/.hoki'),device,args.image,args.expected_version,
                   args.old_partition_sha256,args.image_sha256,pre_write=no_active_capture)
    print(json.dumps(result,indent=2),flush=True)


if __name__=='__main__':
    main()
