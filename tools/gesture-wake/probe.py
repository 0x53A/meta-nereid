#!/usr/bin/env python3
"""Explicit, supervised research trials. No operation occurs on import or --help."""
import argparse
import fcntl
import gzip
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import time
import uuid

ROOT = Path('/var/lib/hoki-gesture-wake')
LOCK = Path('/run/hoki-gesture-wake.lock')
HERE = Path(__file__).resolve().parent
TYPES = {17: 'significant-motion', 26: 'wrist-tilt', 30: 'motion-detect'}
DESCRIPTOR = re.compile(r'^DESCRIPTOR handle=(\d+) type=(\d+) min_delay_us=(-?\d+) max_delay_us=(-?\d+) fifo_reserved=(\d+) fifo_max=(\d+) flags=(\d+)$', re.M)


def run(args, timeout=10, check=True, **kw):
    return subprocess.run(args, timeout=timeout, check=check, text=True, **kw)


def save(path, value):
    with path.open('w') as out:
        json.dump(value, out, indent=2)
        out.write('\n')
        out.flush()
        os.fsync(out.fileno())


def stamp():
    return {'boot_ns': time.clock_gettime_ns(time.CLOCK_BOOTTIME),
            'mono_ns': time.monotonic_ns(), 'wall_ns': time.time_ns()}


def power_status():
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(3)
        client.connect('/run/hoki-powerd/control.sock')
        client.sendall(b'{"version":1,"command":"status"}\n')
        reply = b''
        while b'\n' not in reply:
            part = client.recv(4096)
            if not part or len(reply) + len(part) > 65536:
                raise RuntimeError('invalid powerd reply')
            reply += part
        status = json.loads(reply.split(b'\n')[0])
    if status.get('ok') is not True:
        raise RuntimeError('powerd status failed')
    return status


def validate_power(status, suspend):
    config = status.get('config', {})
    if config.get('enabled') is not False or config.get('sensor_profile') != 'off':
        raise RuntimeError('requires automatic sleep disabled and sensor profile off; no settings changed')
    if suspend and (status.get('sensor_fault') is not False or
                    not isinstance(status.get('inhibitors'), list) or
                    any(x.get('cpu') is not False for x in status['inhibitors'])):
        raise RuntimeError('sensor fault or CPU inhibitor: finish display handoff/background work first')


def candidate(text, typ):
    rows = [dict(zip(('handle', 'type', 'min_delay_us', 'max_delay_us',
                      'fifo_reserved', 'fifo_max', 'flags'), map(int, m)))
            for m in DESCRIPTOR.findall(text)]
    selected = [r for r in rows if r['type'] == typ and r['flags'] & 1]
    if len(selected) != 1:
        raise RuntimeError('requires exactly one current wake-up descriptor for selected type')
    item = selected[0]
    expected_mode = 0 if typ == 1 else (3 if typ == 26 else 2)
    if (item['flags'] >> 1) & 7 != expected_mode:
        raise RuntimeError('unexpected reporting mode; inspect descriptor before proceeding')
    return item


def unit_state(unit):
    return run(['systemctl', 'show', unit, '-p', 'ActiveState', '--value'],
               stdout=subprocess.PIPE).stdout.strip()


def validate_stopped(state, pid):
    if state not in ('inactive', 'failed') or pid != '0':
        raise RuntimeError('sensorfwd is not stopped: state=' + state + ' pid=' + pid)


def validate_pm_test(setting, kernel_config=None):
    if setting is not None:
        if '[none]' not in setting:
            raise RuntimeError('kernel pm_test is not none')
    elif kernel_config is None or '# CONFIG_PM_DEBUG is not set' not in kernel_config.splitlines():
        raise RuntimeError('missing pm_test without proof that CONFIG_PM_DEBUG is disabled')


def preflight(suspend, check_processes=True):
    if os.geteuid() != 0:
        raise RuntimeError('watch root required')
    status = power_status()
    validate_power(status, suspend)
    for unit in ('hoki-health-recording.service', 'hoki-health-profile-recording.service'):
        if unit_state(unit) not in ('inactive', 'failed'):
            raise RuntimeError('active or transitioning capture: ' + unit)
    # These checks supplement, rather than establish, the operator's exclusive handoff.
    processes = run(['ps', '-eo', 'comm='], stdout=subprocess.PIPE).stdout.split()
    if check_processes and any(p.startswith(('hoki-health-rec', 'hoki-ssc-record', 'hoki-suspend-ch')) for p in processes):
        raise RuntimeError('existing recorder/suspend helper; finish its session first')
    if suspend:
        try:
            validate_pm_test(Path('/sys/power/pm_test').read_text())
        except FileNotFoundError:
            with gzip.open('/proc/config.gz', 'rt') as config:
                validate_pm_test(None, config.read())
        if 'mem' not in Path('/sys/power/state').read_text().split():
            raise RuntimeError('mem suspend unavailable')
        if Path('/sys/class/power_supply/battery/status').read_text().strip() != 'Discharging':
            raise RuntimeError('requires discharging battery')
        if Path('/sys/class/android_usb/android0/state').read_text().strip() != 'DISCONNECTED':
            raise RuntimeError('requires USB disconnected')
        if int(Path('/sys/class/net/wlan0/flags').read_text().strip(), 16) & 1:
            raise RuntimeError('first isolated trial requires Wi-Fi administratively down')
        inhibitors = json.loads(run(['busctl', '--json=short', 'call',
            'org.freedesktop.login1', '/org/freedesktop/login1',
            'org.freedesktop.login1.Manager', 'ListInhibitors'], stdout=subprocess.PIPE).stdout)
        for what, who, why, mode, uid, pid in inhibitors['data'][0]:
            if set(what.split(':')) & {'sleep', 'idle'}:
                raise RuntimeError('logind inhibitor: ' + who + ': ' + why)
    return status


def snapshot(directory, label):
    paths = ('/sys/kernel/debug/wakeup_sources', '/sys/kernel/wakeup_reasons/last_resume_reason',
             '/sys/kernel/debug/suspend_stats', '/sys/power/pm_test', '/sys/power/mem_sleep',
             '/sys/class/power_supply/battery/charge_counter',
             '/sys/class/power_supply/battery/current_now',
             '/sys/class/power_supply/battery/status')
    data = {'time': stamp(), 'files': {}}
    for path in paths:
        try:
            data['files'][path] = Path(path).read_text()
        except OSError as exc:
            data['files'][path] = {'unavailable': str(exc)}
    save(directory / (label + '.json'), data)


def recorder_env(directory, typ, raw_accel=False, accel_batch_ms=20000, mode='suspend'):
    # Do not inherit another session's sensor settings or flush directory.
    env = {k: v for k, v in os.environ.items() if not k.startswith('HOKI_')}
    env.update(HOKI_SENSOR_TYPES=str(typ), HOKI_WAKEUP='1', HOKI_BATCH_MS='0',
               HOKI_RECORD_SECONDS='120', HOKI_RECORD_FORMAT='text',
               HOKI_FLUSH_DIR=str(directory / 'control'))
    if raw_accel:
        if accel_batch_ms not in (7000, 20000, 40000):
            raise ValueError('unsupported acceleration batching step')
        env['HOKI_SENSOR_TYPES'] = '1' if mode == 'baseline' else '1,' + str(typ)
        env['HOKI_ACCEL_BATCH_MS'] = str(accel_batch_ms)
    return env


def configured_env(directory, config):
    return recorder_env(directory, config['type'], config.get('raw_accel', False),
                        config.get('accel_batch_ms', 20000), config['mode'])


def validate_capture(path):
    path = Path(path)
    if path.parent != ROOT or not re.fullmatch(r'trial-[0-9a-f-]{36}', path.name):
        raise RuntimeError('invalid trial directory')
    if path.is_symlink() or path.stat().st_uid != 0 or path.stat().st_mode & 0o077:
        raise RuntimeError('trial directory must be private and root-owned')
    return path


def trial(directory):
    directory = validate_capture(directory)
    config = json.loads((directory / 'trial.json').read_text())
    unit = config['unit']
    if run(['systemctl', 'show', unit, '-p', 'MainPID', '--value'],
           stdout=subprocess.PIPE).stdout.strip() != str(os.getpid()):
        raise RuntimeError('run only through launch and its independent systemd cleanup')
    with LOCK.open('a') as lease:
        fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        save(directory / 'preflight.json', preflight(config['mode'] != 'awake'))
        if unit_state('sensorfwd.service') != 'active':
            raise RuntimeError('requires initially active sensorfwd for owned restoration')
        # Durable restoration intent precedes the first service mutation.
        save(directory / 'restore.json', {'sensorfwd_was_active': True})
        run(['systemctl', 'stop', 'sensorfwd.service'], timeout=20)
        state = unit_state('sensorfwd.service')
        pid = run(['systemctl', 'show', 'sensorfwd.service', '-p', 'MainPID', '--value'],
                  stdout=subprocess.PIPE).stdout.strip()
        save(directory / 'sensorfwd-stop.json', {'state': state, 'pid': pid})
        validate_stopped(state, pid)
        # The vendor HAL may exit when its sole sensorfw client disconnects.
        time.sleep(1)
        run(['/usr/sbin/hoki-start-sensors'], timeout=15)
        with (directory / 'descriptors.log').open('w') as out:
            run([str(HERE / 'hoki-health-record'), 'describe'], stdout=out, stderr=out)
        typ = config['type']
        if config['mode'] != 'baseline':
            save(directory / 'selected.json', candidate((directory / 'descriptors.log').read_text(), typ))
        if config.get('raw_accel'):
            save(directory / 'raw-selected.json', candidate((directory / 'descriptors.log').read_text(), 1))
        snapshot(directory, 'before')
        env = configured_env(directory, config)
        (directory / 'control').mkdir(mode=0o700)
        child = None
        with (directory / 'events.txt').open('w') as events, (directory / 'recorder.log').open('w') as log:
            if config['mode'] != 'baseline' or config.get('raw_accel'):
                save(directory / 'activation-intent.json', {'types': env['HOKI_SENSOR_TYPES'],
                     'gesture_latency_ms': 0, 'accel_latency_ms': config.get('accel_batch_ms') if config.get('raw_accel') else None})
                child = subprocess.Popen([str(HERE / 'hoki-health-record'), 'record'],
                                         env=env, stdout=events, stderr=log)
                deadline = time.monotonic() + 10
                while 'READY boottime_ns=' not in (directory / 'recorder.log').read_text():
                    if child.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError('recorder failed readiness')
                    time.sleep(0.05)
                if 'UNAVAILABLE ' in (directory / 'recorder.log').read_text():
                    raise RuntimeError('sensor activation failed')
            print('READY: trial starts after 5 seconds; follow the external action schedule', flush=True)
            time.sleep(5)
            # Repeat changing power/charger/radio/inhibitor checks without mistaking
            # our own recorder for a competing session.
            save(directory / 'entry-preflight.json', preflight(config['mode'] != 'awake', check_processes=False))
            save(directory / 'window-start.json', stamp())
            if config['mode'] == 'awake':
                time.sleep(config['seconds'])
            else:
                with (directory / 'suspend.log').open('w') as out:
                    result = run([str(HERE / 'hoki-suspend-check'), 'mem', str(config['seconds'])],
                                 timeout=config['seconds'] + 8, check=False, stdout=out, stderr=out)
                save(directory / 'suspend-exit.json', {'returncode': result.returncode})
            save(directory / 'window-end.json', stamp())
            snapshot(directory, 'after')
            # Retain events delivered just after resume, then request durable shutdown.
            time.sleep(config.get('post_resume_seconds', 1))
            if child is not None:
                (directory / 'control' / 'stop').touch()
                if child.wait(timeout=15) != 0:
                    raise RuntimeError('recorder failed; retain partial evidence')
        save(directory / 'finished.json', {'time': stamp(), 'capture_finished': True,
                                          'gesture_wake_proven': False})


def cleanup(directory):
    with LOCK.open('a') as lease:
        fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        cleanup_owned(directory)


def cleanup_owned(directory):
    directory = validate_capture(directory)
    if not (directory / 'restore.json').exists():
        return
    config = json.loads((directory / 'trial.json').read_text())
    # ExecStopPost runs after systemd has stopped the complete trial cgroup,
    # including a stuck poll worker. Deactivate only the selected wake-up type.
    activated = (directory / 'activation-intent.json').exists()
    result = {'time': stamp(), 'off_rc': None if activated else 0,
              'restore_rc': None, 'sensorfwd_state': 'unknown'}
    try:
        if activated:
            with (directory / 'cleanup.log').open('a') as out:
                result['off_rc'] = run([str(HERE / 'hoki-health-record'), 'off'],
                    env=configured_env(directory, config), timeout=10,
                    check=False, stdout=out, stderr=out).returncode
    finally:
        try:
            result['restore_rc'] = run(['systemctl', 'start', 'sensorfwd.service'],
                                      timeout=20, check=False).returncode
            result['sensorfwd_state'] = unit_state('sensorfwd.service')
        finally:
            save(directory / 'cleanup.json', result)
        with (directory / 'kernel-journal.txt').open('w') as out:
            run(['journalctl', '-k', '-b', '-n', '300', '--no-pager'],
                check=False, stdout=out, stderr=out)
        with (directory / 'unit-journal.txt').open('w') as out:
            run(['journalctl', '-b', '-u', config['unit'], '--no-pager'],
                check=False, stdout=out, stderr=out)
    if result.get('off_rc', 0) != 0 or result['restore_rc'] != 0 or result['sensorfwd_state'] != 'active':
        raise RuntimeError('cleanup incomplete; inspect saved state before another trial')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest='command', required=True)
    launch = subs.add_parser('launch', help='WATCH ONLY: explicitly start an isolated trial')
    launch.add_argument('--confirm-exclusive-handoff', action='store_true', required=True)
    launch.add_argument('--mode', choices=('awake', 'suspend', 'baseline'), required=True)
    launch.add_argument('--type', type=int, choices=TYPES, default=26)
    launch.add_argument('--seconds', type=int, choices=range(10, 31), default=20)
    launch.add_argument('--action', choices=('shake', 'tilt', 'still', 'walk', 'arm-swing'), required=True)
    launch.add_argument('--raw-accel', action='store_true', help='also record buffered raw XYZ acceleration')
    launch.add_argument('--accel-batch-ms', type=int, choices=(7000, 20000, 40000), default=20000)
    launch.add_argument('--post-resume-seconds', type=int, choices=range(1, 16), default=1,
                        help='retain source data after the suspend/awake window before final flush')
    for name in ('trial', 'cleanup'):
        subs.add_parser(name).add_argument('directory')
    args = parser.parse_args()
    os.umask(0o077)
    if args.command == 'launch':
        preflight(args.mode != 'awake')
        if any(c.isspace() for c in str(HERE)):
            raise RuntimeError('bundle path must not contain whitespace')
        run(['sha256sum', '--check', 'SHA256SUMS'], cwd=HERE)
        for name in ('hoki-health-record', 'hoki-suspend-check'):
            if not os.access(HERE / name, os.X_OK):
                raise RuntimeError('missing built bundle executable: ' + name)
        ROOT.mkdir(mode=0o700, exist_ok=True)
        directory = ROOT / ('trial-' + str(uuid.uuid4()))
        directory.mkdir(mode=0o700)
        unit = 'hoki-gesture-' + directory.name + '.service'
        save(directory / 'trial.json', dict(vars(args), unit=unit,
            boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(), created=stamp()))
        print('Capture: ' + str(directory), flush=True)
        run(['systemd-run', '--unit=' + unit, '--collect', '--property=Type=exec',
             '--property=RuntimeMaxSec=100', '--property=TimeoutStopSec=60',
             '--property=KillMode=control-group', '--property=UMask=0077',
             '--property=ExecStopPost=/usr/bin/python3 ' + str(HERE / 'probe.py') + ' cleanup ' + str(directory),
             '/usr/bin/python3', str(HERE / 'probe.py'), 'trial', str(directory)])
    elif args.command == 'trial':
        trial(args.directory)
    else:
        cleanup(args.directory)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        print('GESTURE TRIAL: ' + str(exc), file=sys.stderr)
        sys.exit(1)
