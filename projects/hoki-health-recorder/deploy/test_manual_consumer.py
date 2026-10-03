"""Ordinary manual recording holds/releases a lease and cannot silently override optical policy."""
import importlib.util
import os
from pathlib import Path
import unittest
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('manual_consumer',Path(__file__).with_name('manual-consumer.py'))
manual=importlib.util.module_from_spec(spec);spec.loader.exec_module(manual)

class ManualConsumerTests(unittest.TestCase):
    def test_normal_manual_recording_never_starts_private_capture(self):
        calls=[]
        class Client:
            def request(self,command='status',**args):calls.append((command,args));return {'ready':True}
            def close(self):calls.append(('close',{}))
        def sleep(_):manual.stopping=True
        manual.stopping=False
        with patch.dict(os.environ,{},clear=True),patch.object(manual,'HealthClient',Client), \
             patch.object(manual.time,'sleep',sleep),patch.object(manual.subprocess,'run') as subprocess:
            manual.run()
        self.assertEqual(calls[0],('acquire',{'profile':'full'}))
        self.assertEqual(calls[-1],('close',{}))
        subprocess.assert_not_called()
    def test_other_consumer_revision_does_not_release_manual_full_lease(self):
        calls=[];states=iter([{'ready':True},{'ready':False,'error':None},{'ready':True}]);ticks=[0]
        class Client:
            def request(self,command='status',**args):
                calls.append(command)
                return {} if command=='acquire' else next(states)
            def close(self):calls.append('close')
        def sleep(_):
            ticks[0]+=1
            if ticks[0]==3:manual.stopping=True
        manual.stopping=False
        with patch.dict(os.environ,{},clear=True),patch.object(manual,'HealthClient',Client),patch.object(manual.time,'sleep',sleep):
            manual.run()
        self.assertEqual(calls,['acquire','status','status','status','close'])
    def test_incompatible_optical_override_is_not_silently_ignored(self):
        with patch.dict(os.environ,{'HOKI_SPO2_POLICY':'continuous'},clear=True):
            with self.assertRaisesRegex(RuntimeError,'Shared collection'):manual.run()
