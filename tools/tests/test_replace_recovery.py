#!/usr/bin/env python3
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

p=Path(__file__).resolve().parents[1] / 'replace-recovery.py'
spec=importlib.util.spec_from_file_location('update',p)
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

def image(kernel,ramdisk=b'unchanged initramfs'):
    header=bytearray(4096);header[:8]=b'ANDROID!'
    struct.pack_into('<10I',header,8,len(kernel),0x8000,len(ramdisk),0x1000000,0,0,0,4096,0,0)
    ident=hashlib.sha1()
    for part in (kernel,ramdisk,b''):
        ident.update(part);ident.update(struct.pack('<I',len(part)))
    header[576:596]=ident.digest()
    pad=lambda b:b+bytes((-len(b))%4096)
    return bytes(header)+pad(kernel)+pad(ramdisk)

class Tests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.p=Path(self.temp.name);self.store=self.p/'store';self.d=self.store/'versions/v1';self.d.mkdir(parents=True)
        (self.store/'selection').write_text('v1 -\n')
        self.old=image(b'old kernel');self.new=image(b'new kernel')
        self.partition=self.old+bytes(m.PARTITION_SIZE-len(self.old))
        self.device=self.p/'device';self.device.write_bytes(self.partition)
        self.input=self.p/'new.img';self.input.write_bytes(self.new)
        (self.d/'rootfs.ext4').write_bytes(b'unchanged root')
        data={'format':1,'version':'v1','rootfs_size':14,'rootfs_sha256':m.sha(b'unchanged root'),'recovery_size':len(self.old),'recovery_sha256':m.sha(self.old)}
        self.meta={'recovery.img':self.old,'recovery.size':(str(len(self.old))+'\n').encode(),'recovery.sha256':(m.sha(self.old)+'\n').encode(),'manifest.json':json.dumps(data).encode()}
        for n,b in self.meta.items():(self.d/n).write_bytes(b)
    def run_update(self,**kw):
        return m.replace(self.store,self.device,self.input,'v1',kw.pop('old_hash',m.sha(self.partition)),m.sha(self.input.read_bytes()),test_device=True,**kw)
    def unchanged(self):
        self.assertEqual(self.device.read_bytes(),self.partition)
        for n,b in self.meta.items():self.assertEqual((self.d/n).read_bytes(),b)
    def test_success_preserves_root(self):
        result=self.run_update()
        self.assertEqual(self.device.read_bytes()[:len(self.new)],self.new)
        self.assertEqual((self.d/'rootfs.ext4').read_bytes(),b'unchanged root')
        self.assertEqual((self.store/'selection').read_text(),'v1 -\n')
        self.assertEqual(json.loads((self.d/'manifest.json').read_text())['recovery_sha256'],m.sha(self.new))
        self.assertEqual((Path(result['backup'])/'recovery-partition.before').read_bytes(),self.partition)
    def test_reject_changed_partition(self):
        with self.assertRaises(ValueError):self.run_update(old_hash='0'*64)
        self.unchanged()
    def test_reject_ramdisk_change(self):
        self.input.write_bytes(image(b'new',b'changed initramfs'))
        with self.assertRaises(ValueError):self.run_update()
        self.unchanged()
    def test_reject_trial(self):
        (self.store/'selection').write_text('v1 trial\n')
        with self.assertRaises(ValueError):self.run_update()
        self.unchanged()
    def test_rollback_after_write(self):
        def fail(stage):
            if stage=='after-write':raise OSError('simulated failure')
        with self.assertRaises(OSError):self.run_update(hook=fail)
        self.unchanged()
    def test_rollback_partial_metadata(self):
        def fail(stage):
            if stage=='metadata-recovery.size':raise OSError('simulated failure')
        with self.assertRaises(OSError):self.run_update(hook=fail)
        self.unchanged()

    def test_rollback_partial_write(self):
        def fail(stage):
            if stage=='write-chunk':raise OSError('simulated partial write')
        with self.assertRaises(OSError):self.run_update(hook=fail)
        self.unchanged()
    def test_rollback_result_write(self):
        original=m.durable
        def fail(path,data):
            if path.name=='result.json':raise OSError('simulated result fsync failure')
            return original(path,data)
        with patch.object(m,'durable',fail):
            with self.assertRaises(OSError):self.run_update()
        self.unchanged()
    def test_explicit_rollback_failure(self):
        def fail(stage):
            if stage=='after-write':raise OSError('simulated commit failure')
        def fail_restore(path,data):raise OSError('simulated restore failure')
        with patch.object(m,'atomic',fail_restore):
            with self.assertRaisesRegex(OSError,'ROLLBACK FAILED: do not reboot'):
                self.run_update(hook=fail)
    def test_reject_rootfs_mismatch(self):
        (self.d/'rootfs.ext4').write_bytes(b'changed root!!')
        with self.assertRaisesRegex(ValueError,'Rootfs checksum mismatch'):self.run_update()
        self.unchanged()

if __name__=='__main__':unittest.main()
