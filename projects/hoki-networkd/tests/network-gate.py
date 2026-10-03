#!/usr/bin/env python3
"""Real systemd lifecycle test with networking in a private namespace.
Run: unshare -Urnm python3 tests/network-gate.py ../target/debug/hoki-networkd
Installs uniquely named temporary runtime units in the host user manager.
"""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
prefix = f'hoki-test-{os.getpid()}-network'
consumer = prefix + '-consumer.service'
online = prefix + '-online.target'
offline = prefix + '-offline.target'
deploy = Path(__file__).resolve().parents[1] / 'deploy'

def command(*args, check=True):
    return subprocess.run(args, check=check, text=True, capture_output=True)

def ctl(*args, check=True):
    return command('systemctl', '--user', *args, check=check)

def wait_for(predicate):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        if predicate(): return
        time.sleep(.03)
    raise AssertionError('Timed out waiting for lifecycle transition')

def state(unit):
    return ctl('show', unit, '-p', 'ActiveState', '--value').stdout.strip()

command('mount', '--make-rprivate', '/')
command('mount', '-t', 'sysfs', 'sysfs', '/sys')
command('ip', 'link', 'set', 'lo', 'up')
command('ip', 'link', 'add', 'tailscale0', 'type', 'dummy')
command('ip', 'link', 'set', 'tailscale0', 'up')
command('ip', 'addr', 'add', '100.64.0.1/24', 'dev', 'tailscale0')
command('ip', 'link', 'add', 'usb0', 'type', 'dummy')
Path('/proc/sys/net/ipv6/conf/usb0/disable_ipv6').write_text('1')
command('ip', 'link', 'set', 'usb0', 'up')

with tempfile.TemporaryDirectory(prefix='hoki-network-systemd-') as tmp:
    root = Path(tmp)
    mock = root / 'consumer.py'
    events = root / 'events'
    mock.write_text('''import os, signal, sys, time
from pathlib import Path
root = Path(__file__).parent
p = root / 'events'
def log(message):
    with p.open('a') as f: f.write(message + '\\n')
if '--cleanup' in sys.argv:
    log('cleanup-begin')
    if (root / 'cleanup-delay').exists(): time.sleep(.8)
    log('cleanup')
    sys.exit(0)
mode = (root / 'mode').read_text().strip() if (root / 'mode').exists() else ''
log('start:' + str(os.getpid()))
if mode == 'fail':
    (root / 'mode').unlink()
    sys.exit(7)
def stop(signum, frame):
    log('term:' + str(os.getpid()))
    if mode == 'stubborn': return
    if mode == 'slow': time.sleep(.8)
    log('stop:' + str(os.getpid()))
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
while True: signal.pause()
''')
    for mode in ('online', 'offline'):
        source = deploy / f'hoki-network-{mode}.target'
        (root / f'{prefix}-{mode}.target').write_text(source.read_text().replace('hoki-network', prefix))
    (root / consumer).write_text(f'''[Unit]
Description=Isolated Hoki network lifecycle test
Requisite={online}
After={online}
StartLimitIntervalSec=10
StartLimitBurst=20

[Service]
ExecStart={sys.executable} {mock}
ExecStopPost={sys.executable} {mock} --cleanup
Restart=on-failure
RestartSec=100ms
TimeoutStopSec=2s

[Install]
WantedBy={online}
''')
    units = [online, offline, consumer]
    gate = None
    def lines(): return events.read_text().splitlines() if events.exists() else []
    def starts(): return [line for line in lines() if line.startswith('start:')]
    def stops(): return [line for line in lines() if line.startswith('stop:')]
    def up(): command('ip', 'link', 'set', 'usb0', 'up')
    def down(): command('ip', 'link', 'set', 'usb0', 'down')
    try:
        ctl('link', '--runtime', *(str(root / unit) for unit in units))
        ctl('enable', '--runtime', consumer)
        ctl('daemon-reload')
        gate = subprocess.Popen([binary, '--user', '--loss-grace-seconds', '1', '--unit-prefix', prefix])
        wait_for(lambda: state(offline) == 'active')
        assert not starts(), 'Loopback/overlay must not activate consumers'
        assert subprocess.run([binary, '--check'], capture_output=True).returncode == 1
        assert ctl('start', consumer, check=False).returncode != 0
        assert state(online) == 'inactive' and state(offline) == 'active'
        ctl('reset-failed', consumer)
        command('ip', 'addr', 'add', '192.0.2.1/24', 'dev', 'usb0')
        wait_for(lambda: len(starts()) == 1)
        assert state(online) == 'active' and state(offline) == 'inactive'
        command('ip', 'addr', 'add', '198.51.100.1/24', 'dev', 'usb0')
        command('ip', 'addr', 'del', '192.0.2.1/24', 'dev', 'usb0')
        time.sleep(.2)
        assert len(starts()) == 1 and not stops()
        down(); time.sleep(.25); up(); time.sleep(1.1)
        assert len(starts()) == 1 and not stops(), 'Transient outage must cancel loss timer'
        down()
        wait_for(lambda: len(stops()) == 1 and lines().count('cleanup') == 1)
        assert state(offline) == 'active' and state(online) == 'inactive'
        up(); wait_for(lambda: len(starts()) == 2)
        command('ip', 'addr', 'del', '198.51.100.1/24', 'dev', 'usb0')
        wait_for(lambda: len(stops()) == 2)
        Path('/proc/sys/net/ipv6/conf/usb0/disable_ipv6').write_text('0')
        command('ip', '-6', 'addr', 'add', 'fd00::1/64', 'dev', 'usb0', 'nodad')
        wait_for(lambda: len(starts()) == 3)
        (root / 'mode').write_text('fail')
        before = len(starts())
        ctl('restart', '--no-block', consumer)
        wait_for(lambda: len(starts()) == before + 2)
        wait_for(lambda: state(consumer) == 'active')
        for mode in ('slow', 'stubborn'):
            (root / 'mode').write_text(mode)
            previous = len(starts())
            ctl('restart', consumer)
            wait_for(lambda: len(starts()) == previous + 1)
            old = int(starts()[-1].split(':')[1])
            before = len(starts())
            before_cleanup = lines().count('cleanup')
            down()
            wait_for(lambda: 'term:' + str(old) in lines())
            up()
            wait_for(lambda: state(online) == 'active')
            time.sleep(.15)
            assert len(starts()) == before, 'No replacement before old consumer exits'
            wait_for(lambda: len(starts()) == before + 1)
            try: os.kill(old, 0)
            except ProcessLookupError: pass
            else: raise AssertionError('Replacement overlaps old consumer')
            assert lines().count('cleanup') == before_cleanup + 1
            assert lines().index('cleanup', lines().index('start:' + str(old))) < lines().index(starts()[-1])
        (root / 'mode').unlink()
        previous = len(starts())
        ctl('restart', consumer)
        wait_for(lambda: len(starts()) == previous + 1)
        (root / 'cleanup-delay').touch()
        before = len(starts())
        before_cleanup = lines().count('cleanup')
        down()
        wait_for(lambda: lines().count('cleanup-begin') > before_cleanup)
        up()
        time.sleep(.15)
        assert len(starts()) == before, 'No replacement during ExecStopPost'
        wait_for(lambda: len(starts()) == before + 1)
        before = len(starts())
        before_cleanup = lines().count('cleanup')
        ctl('start', '--no-block', offline)
        wait_for(lambda: lines().count('cleanup-begin') > before_cleanup)
        ctl('start', '--no-block', online)
        ctl('start', '--no-block', offline)
        wait_for(lambda: lines().count('cleanup') > before_cleanup)
        wait_for(lambda: state(offline) == 'active' and state(consumer) == 'inactive')
        assert len(starts()) == before, 'Latest offline request must override queued recovery'
        (root / 'cleanup-delay').unlink()
        ctl('start', online)
        wait_for(lambda: len(starts()) == before + 1)
        gate.send_signal(signal.SIGTERM)
        assert gate.wait(timeout=5) == 0
        wait_for(lambda: state(consumer) == 'inactive' and state(online) == 'inactive')
        command('ip', 'link', 'del', 'usb0')
    finally:
        if gate is not None and gate.poll() is None:
            gate.terminate()
            gate.wait(timeout=5)
        ctl('stop', *units, check=False)
        ctl('disable', '--runtime', consumer, check=False)
        runtime = Path(os.environ['XDG_RUNTIME_DIR']) / 'systemd/user'
        for unit in units:
            path = runtime / unit
            if path.is_symlink() and path.resolve() == root / unit: path.unlink()
        ctl('daemon-reload')
        ctl('reset-failed', *units, check=False)
print('PASS: real systemd conflicts; offline activation blocked; IPv4/IPv6; overlay exclusion; grace reset; crash restart; recovery during graceful/forced stop and cleanup; rapid reversal; watcher shutdown')
