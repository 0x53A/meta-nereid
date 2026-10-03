import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
from health_broker import Registry, Server
from health_client import HealthClient

class BrokerTests(unittest.TestCase):
    def test_rates_update_existing_lease_and_invalid_request_is_atomic(self):
        with tempfile.TemporaryDirectory() as d:
            r=Registry(Path(d)/'plan.json');r.settings('off')
            r.request('slow','running',{'1':10,'4':10})
            r.request('fast','running',{'1':50})
            v=json.loads(r.plan.read_text())
            self.assertEqual(v['subscriptions'][1]['rates_hz']['1'],10)
            revision=r.revision
            r.request('fast','running',{'1':50});self.assertEqual(r.revision,revision)
            r.request('fast','running',{'1':25});self.assertEqual(r.revision,revision+1)
            before=r.plan.read_bytes()
            for rates in ({'1':True},{'1':float('nan')},{'1':0},{'21':10},{'65572':25},[]):
                with self.assertRaises(ValueError):r.request('fast','running',rates)
                self.assertEqual(r.plan.read_bytes(),before)
            r.release('fast')
            self.assertEqual(json.loads(r.plan.read_text())['subscriptions'],[
                {'profile':'off','rates_hz':{}},{'profile':'running','rates_hz':{'1':10,'4':10}}])

    def test_union_ownership_conflict_and_release(self):
        with tempfile.TemporaryDirectory() as d:
            registry=Registry(Path(d)/'plan.json')
            registry.settings('full')
            registry.request('activity','running')
            with self.assertRaisesRegex(ValueError,'busy'):registry.request('spo2','spo2')
            self.assertEqual(registry.snapshot()['profiles'],['full','running'])
            registry.release('activity')
            self.assertEqual(registry.snapshot()['profiles'],['full'])
            registry.request('spo2','spo2')
            with self.assertRaisesRegex(ValueError,'busy'):registry.request('activity','running')
            self.assertEqual(json.loads((Path(d)/'plan.json').read_text())['profiles'],['full','spo2'])
    def test_socket_disconnect_releases_only_its_consumer(self):
        with tempfile.TemporaryDirectory() as d:
            registry=Registry(Path(d)/'plan.json');registry.settings('daily')
            server=Server(registry,Path(d)/'socket',uid=os.getuid())
            thread=threading.Thread(target=server.serve_forever);thread.start()
            try:
                client=HealthClient(str(Path(d)/'socket'))
                client.request('acquire',profile='running',rates_hz={'1':10})
                self.assertIn('running',registry.snapshot()['profiles'])
                registry.applied={'ready':True,'applied_rates_hz':{'1':50}}
                observer=HealthClient(str(Path(d)/'socket'))
                self.assertEqual(client.request()['applied_rates_hz'],{'1':50})
                self.assertEqual(observer.request()['applied_rates_hz'],{'1':50})
                observer.close()
                client.close()
                for _ in range(50):
                    if registry.snapshot()['profiles']==['daily']:break
                    time.sleep(.01)
                self.assertEqual(registry.snapshot()['profiles'],['daily'])
            finally:server.shutdown();server.server_close();thread.join()
    def test_unknown_profiles_do_not_mutate_plan(self):
        with tempfile.TemporaryDirectory() as d:
            registry=Registry(Path(d)/'plan.json');registry.settings('sleep')
            before=registry.snapshot()
            with self.assertRaises(ValueError):registry.request('x','unknown')
            self.assertEqual(before,registry.snapshot())

if __name__=='__main__':unittest.main()

class PolicyLifecycleTests(unittest.TestCase):
    def test_live_union_changes_do_not_restart_shared_capture(self):
        from unittest.mock import patch
        import health_broker as b
        ticks=[0];calls=[];holder={};states=['daily','full','sleep','off']
        class Power:
            def request(self,command,**fields):
                if command=='status':return {'config':{'sensor_profile':states[ticks[0]]}}
                return {}
            def inhibit(self,*args):pass
            def close(self):pass
        class Server:
            def shutdown(self):pass
            def server_close(self):pass
        active=[False]
        def systemctl(*args):
            calls.append(args)
            if args[0]=='start':active[0]=True
            if args[0]=='stop':active[0]=False
            return 'active' if active[0] else 'inactive'
        def serve(registry):holder['registry']=registry;return Server()
        def sleep(_):
            if ticks[0]==0:holder['registry'].request('run','running')
            if ticks[0]==2:holder['registry'].release('run')
            ticks[0]+=1
        with tempfile.TemporaryDirectory() as d, patch.object(b,'RUNTIME',Path(d)), \
             patch.object(b,'Registry',lambda: Registry(Path(d)/'demands.json')), \
             patch.object(b,'serve',serve),patch.object(b.time,'sleep',sleep):
            b.run_policy(systemctl,Power,lambda:ticks[0]>=4)
        self.assertEqual(sum(c[0]=='start' for c in calls),1)
        self.assertEqual(sum(c[0]=='stop' for c in calls),1)
        self.assertFalse(any('hoki-health-recording.service' in c for c in calls))

    def test_revision_invalidates_old_acknowledgement(self):
        with tempfile.TemporaryDirectory() as d:
            r=Registry(Path(d)/'plan.json');r.settings('full')
            r.applied={'ready':True}
            r.request('activity','running')
            self.assertFalse(r.snapshot()['ready'])

    def test_failed_start_is_not_retried_without_new_demand(self):
        from unittest.mock import patch
        import health_broker as b
        ticks=[0];attempts=[]
        class Power:
            def request(self,*args,**kwargs):return {'config':{'sensor_profile':'full'}}
            def inhibit(self,*args):pass
            def close(self):pass
        class Server:
            def shutdown(self):pass
            def server_close(self):pass
        def systemctl(*args):
            if args[0]=='start':attempts.append(1);raise RuntimeError('storage admission denied')
            return 'inactive'
        with tempfile.TemporaryDirectory() as d, patch.object(b,'RUNTIME',Path(d)), \
             patch.object(b,'Registry',lambda:Registry(Path(d)/'plan.json')), \
             patch.object(b,'serve',lambda _:Server()),patch.object(b.time,'sleep',lambda _:ticks.__setitem__(0,ticks[0]+1)):
            b.run_policy(systemctl,Power,lambda:ticks[0]>=3)
        self.assertEqual(len(attempts),1)
