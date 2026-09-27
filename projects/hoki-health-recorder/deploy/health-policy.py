#!/usr/bin/env python3
"""Apply collection profiles through an owned service, preserving manual captures.

One start attempt per requested profile change. Battery/storage stops and
failed starts do not turn into automatic retry loops or archive churn.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import time
from power_client import PowerClient

UNIT = 'hoki-health-profile-recording.service'
ENV = Path('/run/hoki-health-policy/profile.env')
STATUS = Path('/run/hoki-health-policy/status.json')
stopping = False


def systemctl(*args):
    return subprocess.run(['systemctl', *args], check=True, timeout=150,
                          capture_output=True, text=True).stdout.strip()


def desired(status):
    config = status['config']
    profile = config['sensor_profile']
    if profile not in ('off', 'daily', 'sleep', 'activity', 'full'):
        raise RuntimeError('Invalid sensor profile')
    return profile


def publish(profile, error=None):
    STATUS.write_text(json.dumps(dict(profile=profile, error=error)) + '\n')


def run():
    ENV.parent.mkdir(mode=0o700, exist_ok=True)
    attempted = None
    applied = None
    client = None
    while not stopping:
        try:
            if client is None:
                client = PowerClient()
            status = client.request('status')
            profile = desired(status)
            key = profile
            if key != attempted:
                client.inhibit('sensor profile transition')
                # This unit is exclusively ours. Never stop the manual unit.
                attempted = key
                applied = None
                systemctl('stop', UNIT)
                if stopping:
                    break
                # Stop can take a long time. Superseding requests own the next start.
                profile = desired(client.request('status'))
                attempted = profile
                if profile != 'off':
                    ENV.write_text('HOKI_SENSOR_PROFILE=' + profile + '\n')
                    systemctl('start', UNIT)
                # A request can also change while start blocks. Do not publish or
                # release the transition inhibitor for an obsolete profile.
                if stopping or desired(client.request('status')) != profile:
                    systemctl('stop', UNIT)
                    attempted = None
                    continue
                applied = profile
                publish(profile)
                client.request('inhibit', cpu=False, display=False, reason='sensor profile settled')
            elif applied and applied != 'off':
                active = systemctl('show', UNIT, '-p', 'ActiveState', '--value')
                if active not in ('active', 'activating'):
                    publish(applied, 'Collection stopped: ' + active)
        except Exception as error:
            publish('error', str(error))
            if client is not None:
                client.close()
                client = None
        time.sleep(2)
    if client is not None:
        client.inhibit('sensor policy shutdown')
    try:
        systemctl('stop', UNIT)
    finally:
        if client is not None:
            client.close()


if __name__ == '__main__':
    def stop(_signal, _frame):
        global stopping
        stopping = True
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    os.umask(0o077)
    run()
