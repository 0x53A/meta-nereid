#!/usr/bin/env python3
"""Settings manual recording is a full-profile consumer of the shared daemon."""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
from health_client import HealthClient

stopping = False

def stop(*_):
    global stopping
    stopping = True


def run():
    if os.environ.get('HOKI_BUFFERED_FULL_TRIAL', '0') != '0':
        # Deliberate finite research trials retain their isolation/restore protocol.
        # Existing setup refuses an owned capture; never stop another consumer here.
        helper = '/usr/libexec/hoki-recording-session'
        try:
            subprocess.run([helper, 'prepare'], check=True)
            child = subprocess.Popen([helper, 'run'])
            while child.poll() is None:
                if stopping:
                    child.terminate()
                time.sleep(.2)
            if child.returncode:
                raise RuntimeError('Experimental recorder failed')
        finally:
            subprocess.run([helper, 'cleanup'], check=True)
        return
    if os.environ.get('HOKI_SPO2_POLICY', 'periodic') != 'periodic':
        raise RuntimeError('Shared collection owns periodic SpO2 policy; use an isolated trial for overrides')
    if any(os.environ.get(k) is not None for k in ('HOKI_BUFFERED_FULL_TRIAL_SECONDS','HOKI_BUFFERED_FULL_TRIAL_LATENCY_SECONDS')) or os.environ.get('HOKI_BUFFERED_TRIAL_SELECTION','full') != 'full':
        raise RuntimeError('Buffered options require the explicit trial mode')
    client = HealthClient()
    try:
        client.request('acquire', profile='full')
        deadline = time.monotonic()+110
        notified = False
        last_ready = time.monotonic()
        while not stopping:
            state = client.request()
            if state.get('ready'):
                last_ready = time.monotonic()
            if notified and time.monotonic() - last_ready > 15:
                raise RuntimeError('Sensor recording lost')
            if state.get('ready') and not notified:
                address = os.environ.get('NOTIFY_SOCKET')
                if address:
                    with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as notify:
                        notify.connect('\0'+address[1:] if address.startswith('@') else address)
                        notify.sendall(b'READY=1')
                notified = True
            if not notified and time.monotonic() >= deadline:
                raise RuntimeError('Sensor consumer was not acknowledged')
            time.sleep(1)
    finally:
        client.close()

if __name__ == '__main__':
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    run()
