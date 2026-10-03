import json
import math
from pathlib import Path
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from activity import Activity, metres
from export import export, records

class ActivityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.t = 100
        self.a = Activity(self.temp.name, now=lambda: self.t, boot='test-boot')
    def tearDown(self):
        if not self.a.file.closed: self.a.file.close()
        self.temp.cleanup()
    def fix(self, lon, source=None, fresh=True, accuracy=5):
        self.a.gps(dict(event='dbus', interface='org.freedesktop.Geoclue.Position',
                        fresh_for_session=fresh, arguments=[7, source or 100000+self.t, 52, lon, 20, [6,accuracy,5]]))
    def test_pause_keeps_raw_and_splits_distance_and_active_time(self):
        self.fix(13)
        self.a.command('start'); self.fix(13, source=100101)
        self.t += 5; self.fix(13.0001)
        distance=self.a.distance
        self.a.command('pause'); self.t+=30; self.fix(13.01)
        self.a.command('resume'); self.t+=5; self.fix(13.02)
        self.assertEqual(self.a.distance,distance)
        self.t+=5; self.fix(13.0201); self.a.command('stop')
        self.assertAlmostEqual(self.a.snapshot()['active_seconds'],15)
        self.assertAlmostEqual(self.a.snapshot()['elapsed_seconds'],45)
        rows=list(records(self.a.path))
        self.assertEqual([r['event'] for r in rows if r['event'] in ('start','pause','resume','stop')],['start','pause','resume','stop'])
        self.assertEqual(len([r for r in rows if r['event']=='gps']),6)
        export(self.a.path,Path(self.temp.name)/'run')
        tree=ET.parse(Path(self.temp.name)/'run.gpx')
        self.assertEqual(len(tree.findall('.//{*}trkseg')),2)
    def test_no_cached_inaccurate_duplicate_or_jump_distance(self):
        self.a.command('start'); self.fix(13,fresh=False)
        self.assertFalse(self.a.snapshot()['gps_lock'])
        self.fix(13);self.t+=1;self.fix(13.1,source=100100)
        self.assertEqual(self.a.distance,0)
        self.fix(13.1);self.assertEqual(self.a.distance,0)
        self.t+=1;self.fix(13.2,accuracy=100)
        self.t+=1;self.fix(13.3)
        self.assertEqual(self.a.distance,0)
        self.t+=11;self.fix(13.3001)
        self.assertEqual(self.a.distance,0)
    def test_bad_transitions_and_no_fix_start(self):
        with self.assertRaises(ValueError): self.a.command('resume')
        self.a.command('start')
        self.t+=60;self.a.command('stop')
        self.assertEqual(self.a.snapshot()['distance_m'],0)
        self.assertIsNone(self.a.snapshot()['average_pace_seconds_km'])
        with self.assertRaises(ValueError): self.a.command('pause')
    def test_torn_tail_preserves_prefix_and_corruption_is_rejected(self):
        self.a.command('stop')
        with self.a.path.open('ab') as f:f.write(b'{"incomplete":')
        self.assertEqual(len(list(records(self.a.path))),3)
        with self.a.path.open('ab') as f:f.write(b'bad}\n')
        with self.assertRaises(ValueError):list(records(self.a.path))
    def test_dateline_distance(self):
        self.assertLess(metres((0,179.999),(0,-179.999)),225)
    def test_interrupted_is_never_complete(self):
        self.a.command('start');self.t+=12;self.a.interrupt('failure')
        self.assertEqual(self.a.snapshot()['active_seconds'],12)
        self.assertEqual(list(records(self.a.path))[-1]['event'],'interrupted')

if __name__=='__main__':unittest.main()

class DaemonTests(unittest.TestCase):
    def test_ui_disconnect_is_not_an_activity_stop_and_sensor_ack_required(self):
        from unittest.mock import patch
        import os
        os.environ.setdefault('XDG_RUNTIME_DIR', '/tmp/hoki-test-unused')
        sys.path.insert(0, str(Path(__file__).resolve().parents[2]/'hoki-health-recorder/deploy'))
        import daemon
        class Health:
            ready=False
            def request(self,*args,**kwargs):return dict(ready=self.ready,capture='/test/capture',revision=3)
            def close(self):pass
        with tempfile.TemporaryDirectory() as directory:
            d=daemon.Daemon();d.activity=Activity(directory,boot='test');d.health=Health()
            with self.assertRaisesRegex(ValueError,'Waiting'):d.command({'command':'start'})
            d.health.ready=True;d.command({'command':'start'})
            # Pure status polling and changing UI connections do not mutate session state.
            for _ in range(3):self.assertEqual(d.command({'command':'status'})['activity']['state'],'running')
            d.command({'command':'pause'})
            self.assertEqual(d.activity.state,'paused')
            self.assertFalse(d.activity.file.closed)
            d.command({'command':'resume'});d.command({'command':'stop'})
            self.assertTrue(d.activity.file.closed)
            self.assertIsNone(d.health)
            d.selector.close()

    def test_stop_drains_gps_tail_without_adding_post_stop_distance(self):
        import os, subprocess
        os.environ.setdefault('XDG_RUNTIME_DIR', '/tmp/hoki-test-unused')
        sys.path.insert(0, str(Path(__file__).resolve().parents[2]/'hoki-health-recorder/deploy'))
        import daemon
        with tempfile.TemporaryDirectory() as directory:
            d=daemon.Daemon(); d.activity=Activity(directory,boot='test')
            d.activity.command('start')
            d.gps=subprocess.Popen([sys.executable,'-c',
                'import sys;sys.stdin.readline();print(\'{"event":"session_end","reason":"user_stop"}\',flush=True)'],
                stdin=subprocess.PIPE,stdout=subprocess.PIPE,bufsize=0)
            d.command({'command':'stop'})
            rows=list(records(d.activity.path))
            self.assertEqual([r['event'] for r in rows][-3:],['stop','gps','summary'])
            self.assertEqual(rows[-2]['raw']['event'],'session_end')
            self.assertEqual(d.activity.snapshot()['gps_error'],'')
            self.assertEqual(d.activity.snapshot()['distance_m'],0)
            d.selector.close()
