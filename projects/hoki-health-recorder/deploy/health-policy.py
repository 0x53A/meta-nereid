#!/usr/bin/env python3
"""Settings and application consumers share one dynamically configured capture."""
import os
import signal
import subprocess
from power_client import PowerClient
from health_broker import run_policy

stopping = False


def systemctl(*args):
    return subprocess.run(['systemctl', *args], check=True, timeout=150,
                          capture_output=True, text=True).stdout.strip()


def stop(*_):
    global stopping
    stopping = True


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    os.umask(0o077)
    run_policy(systemctl, PowerClient, lambda: stopping)
