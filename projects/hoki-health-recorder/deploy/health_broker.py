"""Connection-owned sensor consumers. Only the recorder touches HAL demands."""
import json
import math
import fcntl
import os
from pathlib import Path
import pwd
import socket
import socketserver
import struct
import threading
import time
import uuid

PROFILES = ('off', 'daily', 'sleep', 'activity', 'full', 'running', 'spo2')
RUNTIME = Path('/run/hoki-health-policy')
SOCKET = RUNTIME / 'control.sock'
PLAN = RUNTIME / 'demands.json'


def subscription(profile, rates):
    if profile not in ('running', 'spo2', 'full', 'daily', 'sleep', 'activity'):
        raise ValueError('Unsupported app profile')
    if rates is None:
        rates = {}
    allowed = {'full': {'1', '2', '4', '6', '9', '10', '11', '14', '15', '16', '20', '35', '65572'},
               'running': {'1', '4'}, 'activity': {'1', '4'},
               'sleep': {'1'}, 'daily': set(), 'spo2': set()}[profile]
    if not isinstance(rates, dict) or not set(rates).issubset(allowed):
        raise ValueError('rates_hz must name adjustable sensor types in this profile')
    for rate in rates.values():
        if isinstance(rate, bool) or not isinstance(rate, (int, float)) or not math.isfinite(rate) or not 0.1 <= rate <= 1000:
            raise ValueError('Each rate must be finite and between 0.1 and 1000 Hz')
    return dict(profile=profile, rates_hz=dict(rates))


def atomic(path, value):
    temp = path.with_suffix('.tmp')
    with temp.open('w') as f:
        json.dump(value, f)
        f.write('\n')
        f.flush()
        os.fsync(f.fileno())
    os.replace(temp, path)


class Registry:
    def __init__(self, plan=PLAN):
        self.plan = plan
        self.lock = threading.RLock()
        self.consumers = {}
        self.profile = 'off'
        self.revision = 0
        self.epoch = str(uuid.uuid4())
        self.applied = {'ready': False, 'error': 'Recorder not ready'}

    def snapshot(self):
        with self.lock:
            return dict(revision=self.revision, epoch=self.epoch, profiles=[self.profile] + [s['profile'] for s in self.consumers.values()],
                        **self.applied)

    def publish(self):
        self.revision += 1
        self.applied = dict(ready=False, error="Applying sensor requests")
        atomic(self.plan, dict(version=1, epoch=self.epoch, revision=self.revision,
                              profiles=[self.profile] + [s['profile'] for s in self.consumers.values()],
                              subscriptions=[dict(profile=self.profile, rates_hz={})] + list(self.consumers.values())))

    def settings(self, profile):
        if profile not in PROFILES[:5]:
            raise ValueError('Unknown Settings profile')
        with self.lock:
            if self.profile != profile or not self.plan.exists():
                self.profile = profile
                self.publish()

    def request(self, owner, profile, rates=None):
        demand = subscription(profile, rates)
        with self.lock:
            # Refuse both directions: never silently steal an in-flight manual sample.
            others = [p['profile'] for key, p in self.consumers.items() if key != owner]
            if (profile == 'spo2' and 'running' in others) or (profile == 'running' and 'spo2' in others):
                raise ValueError('Optical sensor busy')
            if self.consumers.get(owner) != demand:
                self.consumers[owner] = demand
                self.publish()
            return self.snapshot()

    def release(self, owner):
        with self.lock:
            if self.consumers.pop(owner, None) is not None:
                self.publish()


class Server(socketserver.ThreadingUnixStreamServer):
    daemon_threads = True
    block_on_close = False
    def __init__(self, registry, path=SOCKET, uid=None):
        self.registry = registry
        self.allowed_uid = pwd.getpwnam('ceres').pw_uid if uid is None else uid
        super().__init__(str(path), Handler)


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        _, uid, _ = struct.unpack('3i', self.request.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        if uid not in (0, self.server.allowed_uid):
            return
        owner = str(uuid.uuid4())
        self.request.settimeout(30)  # Clients renew with status; dead clients lose only their request.
        try:
            while True:
                line = self.rfile.readline(4097)
                if not line:
                    break
                if len(line) > 4096 or not line.endswith(b'\n'):
                    break
                try:
                    req = json.loads(line)
                    if not isinstance(req, dict):
                        raise ValueError("Expected request object")
                    cmd = req.get('command')
                    if cmd == 'acquire':
                        state = self.server.registry.request(owner, req.get('profile'), req.get('rates_hz'))
                    elif cmd == 'release':
                        self.server.registry.release(owner)
                        state = self.server.registry.snapshot()
                    elif cmd == 'status':
                        state = self.server.registry.snapshot()
                    else:
                        raise ValueError('Unknown command')
                    reply = dict(ok=True, **state)
                except (ValueError, TypeError) as error:
                    reply = dict(ok=False, error=str(error))
                self.wfile.write((json.dumps(reply) + '\n').encode())
                self.wfile.flush()
        except (OSError, TimeoutError):
            pass
        finally:
            self.server.registry.release(owner)


def serve(registry):
    # systemd owns this runtime directory. A process lock in the policy prevents duplicates.
    SOCKET.unlink(missing_ok=True)
    server = Server(registry)
    os.chmod(RUNTIME, 0o711)
    os.chmod(SOCKET, 0o660)
    os.chown(SOCKET, 0, pwd.getpwnam('ceres').pw_gid)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def run_policy(systemctl, PowerClient, stopping):
    RUNTIME.mkdir(mode=0o700, exist_ok=True)
    guard = (RUNTIME / "policy.lock").open("a")
    fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
    registry = Registry()
    registry.settings('off')
    server = serve(registry)
    client = None
    attempted = None
    owned = False
    try:
        while not stopping():
            try:
                if client is None:
                    client = PowerClient()
                registry.settings(client.request('status')['config']['sensor_profile'])
                snap = registry.snapshot()
                wanted = any(p != 'off' for p in snap['profiles'])
                state = systemctl('show', 'hoki-health-profile-recording.service', '-p', 'ActiveState', '--value')
                owned = state in ('active', 'activating')
                if wanted and state not in ('active', 'activating') and attempted != snap['revision']:
                    attempted = snap['revision']
                    client.inhibit('sensor consumer startup')
                    (RUNTIME / 'profile.env').write_text('HOKI_SENSOR_PROFILE=full\nHOKI_HEALTH_DEMAND_FILE=' + str(PLAN) + '\n')
                    systemctl('start', 'hoki-health-profile-recording.service')
                    owned = True
                elif not wanted and state in ('active', 'activating'):
                    client.inhibit('sensor consumer shutdown')
                    systemctl('stop', 'hoki-health-profile-recording.service')
                    owned = False
                current = {}
                session_path = Path('/run/hoki-health-profile-recording/session.json')
                if wanted and session_path.exists():
                    session = json.loads(session_path.read_text())
                    # This is a root-owned runtime record, never a client-supplied path.
                    capture = Path('/var/lib/hoki-health-recordings') / session['id'] / 'hal'
                    ack = capture / 'broker-status.json'
                    if ack.exists():
                        current = json.loads(ack.read_text())
                        current['capture'] = str(capture)
                        # A stale previous controller must never acknowledge a new consumer.
                        age = time.clock_gettime(time.CLOCK_BOOTTIME) - current.get('boottime_seconds', 0)
                        current['ready'] = (state == 'active' and session.get('phase') == 'running' and 0 <= age < min(22, max(5, current.get('status_valid_seconds', 5))) and
                                            current.get('revision') == snap['revision'] and current.get('epoch') == registry.epoch)
                with registry.lock:
                    registry.applied = dict(ready=False, error='Waiting for recorder')
                    registry.applied.update(current)
                    if current.get('revision') != registry.revision:
                        registry.applied['ready'] = False
                    registry.applied.pop('revision', None)
                    registry.applied.pop('epoch', None)
                client.request('inhibit', cpu=False, display=False, reason='sensor consumers settled')
            except Exception as error:
                with registry.lock:
                    registry.applied = dict(ready=False, error=str(error))
                if client:
                    client.close()
                    client = None
            time.sleep(1)
    finally:
        server.shutdown()
        server.server_close()
        if owned:
            systemctl('stop', 'hoki-health-profile-recording.service')
        if client:
            client.close()
        guard.close()
