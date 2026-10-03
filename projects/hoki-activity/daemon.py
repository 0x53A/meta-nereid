#!/usr/bin/env python3
"""User-service activity owner. GUI disconnect never ends a workout."""
import fcntl
import json
import os
from pathlib import Path
import selectors
import signal
import socket
import subprocess
import sys
import time

from activity import Activity, clock
from health_client import HealthClient
from power_client import PowerClient

ROOT = Path(os.environ.get('XDG_DATA_HOME', str(Path.home() / '.local/share'))) / 'hoki-activities'
RUNTIME = Path(os.environ['XDG_RUNTIME_DIR']) / 'hoki-activity'


class Daemon:
    def __init__(self):
        self.activity = None
        self.health = None
        self.power = None
        self.gps = None
        self.health_status = {}
        self.error = ''
        self.clients = {}
        self.selector = selectors.DefaultSelector()
        self.last_heart = None
        self.gps_buffer = b''
        self.last_ready = None
        self.last_sensor_state = None

    def release(self):
        if self.gps:
            try:
                self.selector.unregister(self.gps.stdout)
            except KeyError:
                pass
            tail = b''
            if self.gps.poll() is None:
                try:
                    tail, _ = self.gps.communicate(input=b'stop\n', timeout=3)
                except (OSError, subprocess.TimeoutExpired):
                    self.gps.terminate()
                    try:
                        tail, _ = self.gps.communicate(timeout=2)
                    except subprocess.TimeoutExpired:
                        self.gps.kill()
                        tail, _ = self.gps.communicate()
            else:
                tail = self.gps.stdout.read() or b''
            if tail and self.activity and not self.activity.file.closed:
                self.gps_records(tail)
            self.gps.stdout.close()
            self.gps.stdin.close()
            self.gps = None
        if self.health:
            self.health.close()
            self.health = None
        if self.power:
            self.power.close()
            self.power = None

    def command(self, request):
        if not isinstance(request, dict):
            raise ValueError("Expected request object")
        command = request.get('command')
        if command == 'prepare':
            if self.activity and self.activity.state not in ('stopped', 'interrupted'):
                raise ValueError('An activity is already open')
            if request.get('profile') != 'running':
                raise ValueError('Choose Running')
            self.error = ''
            self.last_heart = None
            try:
                self.power = PowerClient()
                # GPS continuity through system suspend is not verified. Keep CPU awake,
                # never force the display on; battery cost is explicit in README.
                self.power.inhibit('running activity GPS capture')
                self.health = HealthClient()
                self.health_status = self.health.request('acquire', profile='running')
                self.activity = Activity(ROOT)
                self.gps_buffer = b''
                self.last_ready = None
                self.last_sensor_state = None
                self.gps = subprocess.Popen(['/usr/lib/hoki-activity', '--gps'], stdin=subprocess.PIPE,
                                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0)
                os.set_blocking(self.gps.stdout.fileno(), False)
                self.selector.register(self.gps.stdout, selectors.EVENT_READ, 'gps')
            except Exception:
                if self.activity and self.activity.state == 'prepared':
                    self.activity.interrupt('preparation_failed')
                self.release()
                raise
        elif command in ('start', 'pause', 'resume', 'stop'):
            if not self.activity:
                raise ValueError('Select an activity first')
            if command in ('start', 'resume'):
                if not self.health:
                    raise ValueError('Sensor recording unavailable')
                self.health_status = self.health.request()
                self.activity.reference(self.health_status)
                if not self.health_status.get('ready'):
                    raise ValueError('Waiting for sensor recording')
            self.activity.command(command, finalize=command != 'stop')
            if command == 'stop':
                self.release()
                self.activity.finalize()
        elif command != 'status':
            raise ValueError('Unknown command')
        return self.status()

    def status(self):
        state = self.activity.snapshot() if self.activity else dict(state='idle', track=[])
        return dict(ok=True, activity=state, sensors=self.health_status, error=self.error)

    def tick(self):
        if not self.health:
            return
        self.health_status = self.health.request()
        self.activity.reference(self.health_status)
        heart = self.health_status.get('heart_rate')
        if heart and heart != self.last_heart:
            self.activity.write('heart_rate', sample=heart)
            self.last_heart = heart
        # Leases are connection-scoped; renew even while the GUI is closed.
        self.power.request('status')
        sensor_state = (self.health_status.get('ready'), self.health_status.get('revision'), self.health_status.get('optical_window'))
        if sensor_state != self.last_sensor_state:
            self.activity.write('sensor_state', durable=True, ready=sensor_state[0], revision=sensor_state[1], optical_window=sensor_state[2])
            self.last_sensor_state = sensor_state
        if not self.health_status.get('ready'):
            if self.last_ready is not None and clock() - self.last_ready > 15:
                raise RuntimeError('Sensor recording lost')
            self.error = self.health_status.get('error') or 'Sensor recording unavailable'
        else:
            self.last_ready = clock()
            self.error = ''
        if self.activity.file.tell() >= 128 * 1024 * 1024:
            raise RuntimeError('Activity journal reached storage limit')
        space = os.statvfs(ROOT)
        if space.f_bavail * space.f_frsize < 32 * 1024 * 1024:
            raise RuntimeError('Activity storage reserve reached')
        self.activity.file.flush()
        os.fsync(self.activity.file.fileno())

    def gps_ready(self):
        chunk = os.read(self.gps.stdout.fileno(), 65536)
        if not chunk:
            self.selector.unregister(self.gps.stdout)
            self.activity.gps_error = 'GPS unavailable; sensors still recording'
            self.activity.last_fix = None
            self.activity.previous = None
            self.activity.write('gps_unavailable', durable=True)
            return
        self.gps_records(chunk)

    def gps_records(self, chunk):
        self.gps_buffer += chunk
        if len(self.gps_buffer) > 1024 * 1024:
            raise RuntimeError('GPS bridge record exceeds limit')
        while b'\n' in self.gps_buffer:
            line, self.gps_buffer = self.gps_buffer.split(b'\n', 1)
            self.activity.gps(json.loads(line))

    def run(self):
        RUNTIME.mkdir(mode=0o700, parents=True, exist_ok=True)
        lock = (RUNTIME / 'owner.lock').open('a')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        path = RUNTIME / 'control.sock'
        path.unlink(missing_ok=True)
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(str(path))
        os.chmod(path, 0o600)
        server.listen(8)
        server.setblocking(False)
        self.selector.register(server, selectors.EVENT_READ, 'server')
        next_tick = clock()
        try:
            while True:
                for key, _ in self.selector.select(timeout=max(0, min(1, next_tick-clock()))):
                    if key.data == 'server':
                        client, _ = server.accept()
                        client.settimeout(0.2)
                        if len(self.clients) >= 8:
                            client.close()
                            continue
                        self.clients[client] = b''
                        self.selector.register(client, selectors.EVENT_READ, 'client')
                    elif key.data == 'gps':
                        self.gps_ready()
                    else:
                        client = key.fileobj
                        try:
                            chunk = client.recv(4096)
                            if not chunk:
                                raise EOFError()
                            self.clients[client] += chunk
                            if len(self.clients[client]) > 4096:
                                raise EOFError()
                            if b'\n' in self.clients[client]:
                                line, rest = self.clients[client].split(b'\n', 1)
                                self.clients[client] = rest
                                try:
                                    reply = self.command(json.loads(line))
                                except (ValueError, RuntimeError, OSError) as error:
                                    reply = dict(ok=False, error=str(error))
                                client.sendall((json.dumps(reply, allow_nan=False)+'\n').encode())
                        except (OSError, EOFError):
                            self.selector.unregister(client)
                            self.clients.pop(client)
                            client.close()
                if clock() >= next_tick:
                    try:
                        self.tick()
                    except Exception as error:
                        self.error = str(error)
                        if self.activity and self.activity.state not in ('stopped', 'interrupted'):
                            self.activity.interrupt('collection_failed')
                        self.release()
                    next_tick = clock()+1
        finally:
            if self.activity and self.activity.state not in ('stopped', 'interrupted'):
                self.activity.interrupt('daemon_exit')
            self.release()
            server.close()
            path.unlink(missing_ok=True)


if __name__ == '__main__':
    os.umask(0o077)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    Daemon().run()
