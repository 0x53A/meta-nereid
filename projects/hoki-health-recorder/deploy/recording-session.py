#!/usr/bin/env python3
"""Own a manually requested HAL capture and its temporary sensorfw setup."""
import json
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import sys
import time

STATE = Path('/var/lib/hoki-health-recordings')
RUNTIME = Path('/run/hoki-health-recording')
DROPIN = Path('/run/systemd/system/sensorfwd.service.d/80-health-recording.conf')
RECORDER = '/usr/bin/hoki-health-recorder'
BATTERY = Path('/sys/class/power_supply/battery')
LIMIT = 1024 * 1024 * 1024
RESERVE = 256 * 1024 * 1024
CUTOFF = 15


def capture_budget(available):
    budget = min(LIMIT, available - RESERVE - 16 * 1024 * 1024)
    budget = budget // (1024 * 1024) * (1024 * 1024)
    if budget < 128 * 1024 * 1024:
        raise RuntimeError('Not enough free space for a new recording')
    return budget


def command(*args, timeout=30):
    return subprocess.run(args, check=True, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=timeout).stdout.strip()


def private_directory(path):
    path.mkdir(mode=0o700, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
        raise RuntimeError(f'Private owned directory required: {path}')


def save(path, value):
    temporary = path.with_suffix('.tmp')
    with temporary.open('w') as stream:
        json.dump(value, stream, sort_keys=True)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def battery():
    values = {}
    for line in (BATTERY / 'uevent').read_text().splitlines():
        key, sep, value = line.partition('=')
        if sep:
            values[key.removeprefix('POWER_SUPPLY_').lower()] = value
    capacity = int(values['capacity'])
    if not 0 <= capacity <= 100 or values.get('status') not in (
            'Charging', 'Discharging', 'Full', 'Not charging'):
        raise RuntimeError('Battery state unavailable')
    values['boottime_seconds'] = time.clock_gettime(time.CLOCK_BOOTTIME)
    return values


def battery_low(values):
    return values['status'] != 'Charging' and int(values['capacity']) <= CUTOFF


def load_session():
    path = RUNTIME / 'session.json'
    if not path.exists():
        return None
    session = json.loads(path.read_text())
    if not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', session['id']):
        raise RuntimeError('Invalid recording identity')
    if session['boot_id'] != Path('/proc/sys/kernel/random/boot_id').read_text().strip():
        raise RuntimeError('Recording identity belongs to another boot')
    return session


def archive(session):
    return STATE / session['id']


def socket_path(session):
    return archive(session) / 'run' / 'control'


def dropin_contents(session):
    return (f'# hoki-health-recording owner={session["id"]}\n[Service]\n'
            f'Environment=HOKI_RECORDING_SOCKET={socket_path(session)}\n')


def persist(session):
    save(archive(session) / 'session.json', session)
    save(RUNTIME / 'session.json', session)
    save(STATE / 'latest.json', session)


def prepare():
    private_directory(STATE)
    private_directory(RUNTIME)
    if (RUNTIME / 'session.json').exists() or DROPIN.exists():
        raise RuntimeError('An existing recording setup requires cleanup')
    environment = command('systemctl', 'show', 'sensorfwd.service', '-p', 'Environment', '--value')
    if 'HOKI_RECORDING_SOCKET=' in environment:
        raise RuntimeError('Another recorder owns the sensorfw configuration')
    # Respect external runtime capture overrides rather than restarting their HAL.
    if battery_low(battery()):
        raise RuntimeError('Charge above 15% before recording')
    space = os.statvfs(STATE)
    budget = capture_budget(space.f_bavail * space.f_frsize)
    session = dict(id=Path('/proc/sys/kernel/random/uuid').read_text().strip(), phase='preparing',
                   boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                   scope='HAL only; no SSC or suspend policy', battery_cutoff_percent=CUTOFF,
                   limit_bytes=budget, reserve_bytes=RESERVE,
                   started_boottime_seconds=time.clock_gettime(time.CLOCK_BOOTTIME))
    private_directory(archive(session))
    private_directory(archive(session) / 'run')
    persist(session)  # Save ownership before the first sensorfw mutation.
    DROPIN.parent.mkdir(parents=True, exist_ok=True)
    staged = DROPIN.parent / f'.health-recording-{session["id"]}.tmp'
    with staged.open('x') as stream:
        stream.write(dropin_contents(session))
        stream.flush()
        os.fsync(stream.fileno())
    # Install complete bytes without replacing another owner's file. A stop
    # during setup must not leave a half-written override we cannot identify.
    os.link(staged, DROPIN)
    staged.unlink()
    command('systemctl', 'daemon-reload')
    command('systemctl', 'restart', 'sensorfwd.service', timeout=45)
    deadline = time.monotonic() + 30
    while True:
        try:
            loaded = command('busctl', '--timeout=2', 'call', 'com.nokia.SensorService',
                             '/SensorManager', 'local.SensorManager', 'loadPlugin',
                             's', 'accelerometersensor', timeout=4)
            if loaded != 'b true':
                raise RuntimeError('Sensor plugin unavailable')
            if not socket_path(session).is_socket():
                requested = command('busctl', '--timeout=3', 'call', 'com.nokia.SensorService',
                                    '/SensorManager', 'local.SensorManager', 'requestSensor',
                                    'sx', 'accelerometersensor', '0', timeout=5)
                if not requested.startswith('i ') or int(requested[2:]) < 0:
                    raise RuntimeError('Sensor adaptor initialization failed')
            if socket_path(session).is_socket():
                break
        except (subprocess.SubprocessError, RuntimeError):
            if time.monotonic() >= deadline:
                raise
        if time.monotonic() >= deadline:
            raise RuntimeError('Recorder socket did not appear')
        time.sleep(0.5)
    session['phase'] = 'prepared'
    persist(session)


def run():
    session = load_session()
    if not session or session['phase'] != 'prepared':
        raise RuntimeError('Recording was not prepared')
    if DROPIN.read_text() != dropin_contents(session):
        raise RuntimeError('Recording configuration ownership changed')
    requested_stop = []
    child = None

    def stop(signum, _frame):
        requested_stop.append(signum)
        if child is not None and child.poll() is None:
            child.send_signal(signal.SIGTERM)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    environment = dict(os.environ, HOKI_HAL_LIMIT_BYTES=str(session['limit_bytes']))
    # NotifyAccess=all permits the controller's existing READY notification;
    # the service is not active merely because this supervisor started.
    child = subprocess.Popen([RECORDER, str(socket_path(session)),
                              str(archive(session) / 'hal'), '0'], env=environment)
    session['phase'] = 'running'
    session['controller_pid'] = child.pid
    reason = 'controller_exit'
    monitor_error = None
    try:
        persist(session)
        with (archive(session) / 'battery.jsonl').open('a') as telemetry:
            while child.poll() is None:
                if requested_stop:
                    reason = 'requested_stop'
                    child.send_signal(signal.SIGTERM)
                    break
                values = battery()
                telemetry.write(json.dumps(values, sort_keys=True) + '\n')
                telemetry.flush()
                if battery_low(values):
                    reason = 'low_battery'
                    child.send_signal(signal.SIGTERM)
                    break
                written = sum(path.stat().st_size for path in (archive(session) / 'hal').glob('events-*.bin'))
                if written >= session['limit_bytes'] - 8 * 1024 * 1024:
                    reason = 'storage_budget'
                    child.send_signal(signal.SIGTERM)
                    break
                try:
                    child.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    pass
            if requested_stop:
                reason = 'requested_stop'
    except Exception as error:
        reason = 'monitor_failed'
        monitor_error = str(error)
        if child.poll() is None:
            child.send_signal(signal.SIGTERM)
    try:
        result = child.wait(timeout=75)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()
        result = 1
        reason = 'controller_stop_timeout'
    session.update(phase='stopped' if result == 0 and monitor_error is None else 'failed',
                   stop_reason=reason, controller_exit=result, monitor_error=monitor_error)
    persist(session)
    return 0 if result == 0 and monitor_error is None else 1


def cleanup():
    session = load_session()
    if session is None:
        return
    if not DROPIN.exists():
        # No configuration was installed; never restart someone else's sensorfw.
        return
    if DROPIN.read_text() != dropin_contents(session):
        raise RuntimeError('Refusing cleanup of another sensorfw configuration')
    controller = archive(session) / 'hal' / 'controller.json'
    if controller.exists() and socket_path(session).is_socket():
        try:
            output = command(RECORDER, '--cleanup', str(socket_path(session)),
                             str(controller.parent), timeout=45)
            (archive(session) / 'cleanup.txt').write_text(output + '\n')
        except subprocess.SubprocessError as error:
            session['cleanup_error'] = str(error)
    DROPIN.unlink()
    command('systemctl', 'daemon-reload')
    command('systemctl', 'restart', 'sensorfwd.service', timeout=45)
    session['sensorfw_restored'] = True
    session['finished_boottime_seconds'] = time.clock_gettime(time.CLOCK_BOOTTIME)
    if session['phase'] in ('preparing', 'prepared', 'running'):
        session['phase'] = 'interrupted'
    persist(session)
    if session.get('cleanup_error'):
        raise RuntimeError('Capture recovery failed; sensorfw restored, archive retained')


if __name__ == '__main__':
    os.umask(0o077)
    try:
        if os.geteuid() != 0 or len(sys.argv) != 2:
            raise RuntimeError('Root and one operation required')
        operation = {'prepare': prepare, 'run': run, 'cleanup': cleanup}[sys.argv[1]]
        sys.exit(operation() or 0)
    except Exception as error:
        print(f'Health recording: {error}', file=sys.stderr)
        sys.exit(1)
