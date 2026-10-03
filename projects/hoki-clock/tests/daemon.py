#!/usr/bin/env python3
"""Run under dbus-run-session. Never uses real wake alarms or watch hardware."""
import json, os, pathlib, subprocess, tempfile, time
bin_dir = pathlib.Path(__file__).resolve().parents[2] / 'target/debug'
with tempfile.TemporaryDirectory(prefix='hoki-clock-test-') as directory:
    state = pathlib.Path(directory) / 'state.json'
    env = dict(os.environ, HOKI_CLOCK_STATE=str(state))
    def start():
        p = subprocess.Popen([str(bin_dir/'hoki-clockd'), '--test-no-wake'], env=env)
        for _ in range(100):
            if p.poll() is not None: raise AssertionError('daemon exited')
            try:
                command('snapshot')
                return p
            except subprocess.CalledProcessError: time.sleep(.03)
        raise AssertionError('service never appeared')
    def command(op, **kwargs):
        result=subprocess.run([str(bin_dir/'hoki-clock'),'--command',json.dumps(dict(op=op,**kwargs))],check=True,capture_output=True,text=True,timeout=5)
        return json.loads(result.stdout)
    p=start()
    try:
        result=command('timer-add',seconds=3,label='Restart test')
        timer=result['timers'][0]['id']
        command('stopwatch-start')
        command('timer-pause',id=timer)
        paused=command('snapshot')['timers'][0]['remaining']
        time.sleep(.15)
        assert command('snapshot')['timers'][0]['remaining']==paused
        command('timer-resume',id=timer)
        p.terminate();p.wait(timeout=5)
        p=start()
        assert command('snapshot')['timers'][0]['remaining']<=paused
        # No Snapshot call may trigger expiry: require the scheduler to persist it.
        for _ in range(100):
            if json.loads(state.read_text())['timers'][0]['ringing']: break
            time.sleep(.05)
        else: raise AssertionError('scheduler did not persist timer expiry')
        result=command('timer-dismiss',id=timer)
        assert not result['ringing'] and not result['timers']
        result=command('stopwatch-pause')
        elapsed=result['stopwatch']['elapsed']
        assert elapsed>=2500
        time.sleep(.1)
        assert command('snapshot')['stopwatch']['elapsed']==elapsed
        command('stopwatch-reset')
        command('alarm-add',hour=7,minute=30,days=31)
        try: command('timer-add',seconds=0)
        except subprocess.CalledProcessError: pass
        else: raise AssertionError('invalid zero timer accepted')
        assert len(command('snapshot')['alarms'])==1
        p.terminate();p.wait(timeout=5)
        p=start()
        assert len(command('snapshot')['alarms'])==1
        print('PASS: private D-Bus, independent expiry, restart persistence, pause/resume, stopwatch, alarms, invalid input')
    finally:
        p.terminate();p.wait(timeout=5)
