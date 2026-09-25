import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('update',Path(__file__).resolve().parents[1]/'update-rootfs.py')
u=importlib.util.module_from_spec(spec);spec.loader.exec_module(u)

class UpdateTests(unittest.TestCase):
    def exercise(self, boot='new\nv1\n', health=0, available='5000000', timeout=40, alias=None):
        now=[0.0]; calls=[]; confirmed=[]
        def sleep(seconds):now[0]+=seconds
        def run(args,**kw):
            calls.append(args)
            command=args[-1]
            output='';code=0
            if args[0]=='ssh':
                if command.startswith('df '):output=available+'\n'
                elif command.startswith('du '):output='0\n'
                elif command=='cat /proc/sys/kernel/random/boot_id':output='old\n'
                elif command.startswith('cat /proc/sys/kernel/random/boot_id;'):output=boot
                elif command.startswith('for unit '):code=health
                elif command=='hoki-rootfs confirm':confirmed.append(now[0])
            return subprocess.CompletedProcess(args,code,output,'')
        with tempfile.TemporaryDirectory() as d:
            bundle=Path(d)/'v1';bundle.mkdir()
            (bundle/'manifest.json').write_text(json.dumps({'version':'v1','rootfs_size':1024,'recovery_size':1024}))
            args=['update-rootfs',str(bundle),'--host','root@watch','--reboot','--timeout',str(timeout)]
            if alias:
                args += ['--host-key-alias', alias]
            with patch.object(sys,'argv',args), patch.object(u.subprocess,'run',side_effect=run), patch.object(u.time,'monotonic',side_effect=lambda:now[0]), patch.object(u.time,'sleep',side_effect=sleep), contextlib.redirect_stdout(io.StringIO()):
                error=None
                try:u.main()
                except SystemExit as e:error=str(e)
        return calls,confirmed,error

    def test_new_boot_requires_sustained_health(self):
        calls,confirmed,error=self.exercise()
        self.assertIsNone(error)
        self.assertEqual(len(confirmed),1)
        self.assertGreaterEqual(confirmed[0],18)
        health=next(c[-1] for c in calls if c[-1].startswith('for unit '))
        self.assertNotIn('sshd',health)  # socket-activated SSH is sufficient

    def test_destination_controls_default_host_identity(self):
        calls,_,error=self.exercise()
        self.assertIsNone(error)
        ssh=next(c for c in calls if c[0]=='ssh')
        self.assertEqual(ssh[-2],'root@watch')
        self.assertFalse(any(a.startswith('HostKeyAlias=') for a in ssh))

    def test_explicit_identity_alias_is_preserved(self):
        calls,_,error=self.exercise(alias='watch.local')
        self.assertIsNone(error)
        ssh=next(c for c in calls if c[0]=='ssh')
        self.assertIn('HostKeyAlias=watch.local',ssh)

    def test_old_boot_is_not_confirmed(self):
        _,confirmed,error=self.exercise(boot='old\nv1\n',timeout=9)
        self.assertFalse(confirmed)
        self.assertIn('timed out',error)

    def test_fallback_version_is_not_confirmed(self):
        _,confirmed,error=self.exercise(boot='new\nprevious\n')
        self.assertFalse(confirmed)
        self.assertIn('different version',error)

    def test_failed_core_service_is_not_confirmed(self):
        _,confirmed,error=self.exercise(health=1,timeout=9)
        self.assertFalse(confirmed)
        self.assertIn('timed out',error)

    def test_low_space_stops_before_upload(self):
        calls,confirmed,error=self.exercise(available='1')
        self.assertFalse(any(c[0]=='rsync' for c in calls))
        self.assertFalse(confirmed)
        self.assertIn('Insufficient',error)

if __name__=='__main__':unittest.main()
