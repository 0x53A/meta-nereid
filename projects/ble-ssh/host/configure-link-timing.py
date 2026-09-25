#!/usr/bin/env python3
"""Reversible, peer-specific BlueZ timing experiment. Never print pairing data."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile

SECTION = '[ConnectionParameters]\nMinInterval=36\nMaxInterval=36\nLatency=0\nTimeout=500\n'

def section_bounds(text):
    headers = list(re.finditer(r'(?m)^\[([^\]\r\n]+)\]\r?$', text))
    matches = [i for i, h in enumerate(headers) if h.group(1) == 'ConnectionParameters']
    if len(matches) > 1:
        raise ValueError('Duplicate connection parameter sections; refusing to edit')
    if not matches:
        return len(text), len(text)
    i = matches[0]
    return headers[i].start(), headers[i+1].start() if i+1 < len(headers) else len(text)

def replace_section(text, replacement):
    start, end = section_bounds(text)
    old = text[start:end]
    if start == len(text) and replacement and text and not text.endswith('\n'):
        raise ValueError('Unterminated storage file; refusing to edit')
    return text[:start] + replacement + text[end:], old

def atomic_write(path, data, metadata):
    fd, temp = tempfile.mkstemp(prefix='.hoki-timing-', dir=path.parent)
    try:
        os.fchmod(fd, stat.S_IMODE(metadata.st_mode))
        os.fchown(fd, metadata.st_uid, metadata.st_gid)
        with os.fdopen(fd, 'w') as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(temp, path)
    finally:
        if os.path.exists(temp):
            os.unlink(temp)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--adapter', required=True, help='Host Bluetooth adapter address')
    parser.add_argument('--device', required=True, help='Paired watch Bluetooth address')
    parser.add_argument('action', nargs='?', choices=('enable', 'disable'), default='enable')
    args = parser.parse_args()
    for address in (args.adapter, args.device):
        if not re.fullmatch(r'(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}', address):
            raise ValueError('Expected a Bluetooth address containing six hexadecimal octets')
    peer = Path('/var/lib/bluetooth') / args.adapter.upper() / args.device.upper()
    action = args.action
    if os.geteuid() != 0:
        raise PermissionError('Run this helper with sudo')
    with open('/run/lock/hoki-watch-link-timing.lock', 'w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        info, backup = peer / 'info', peer / 'hoki-link-timing-backup.json'
        if info.is_symlink() or not info.is_file() or backup.is_symlink():
            raise ValueError('Expected regular BlueZ storage files')
        if (action == 'enable') == backup.exists():
            raise ValueError('Already enabled' if action == 'enable' else 'No timing backup to restore')
        subprocess.run(['systemctl', 'is-active', '--quiet', 'bluetooth.service'], check=True)
        state = subprocess.check_output(['systemctl', 'show', '-p', 'LoadState', '--value', 'bluetooth.service'], text=True).strip()
        if state != 'loaded':
            raise ValueError('Bluetooth unit is not normally loaded; refusing to change masking state')
        subprocess.run(['systemctl', 'mask', '--runtime', 'bluetooth.service'], check=True)
        original = None
        try:
            subprocess.run(['systemctl', 'stop', 'bluetooth.service'], check=True)
            original = info.read_bytes().decode('utf-8')
            metadata = info.stat()
            replacement = SECTION if action == 'enable' else json.loads(backup.read_text())['section']
            updated, previous = replace_section(original, replacement)
            if action == 'enable':
                fd = os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                with os.fdopen(fd, 'w') as f:
                    json.dump({'section': previous}, f)
                    f.flush()
                    os.fsync(f.fileno())
            atomic_write(info, updated, metadata)
            subprocess.run(['systemctl', 'unmask', '--runtime', 'bluetooth.service'], check=True)
            subprocess.run(['systemctl', 'start', 'bluetooth.service'], check=True)
        except BaseException:
            if original is not None:
                atomic_write(info, original, metadata)
                if action == 'enable' and backup.exists():
                    backup.unlink()
            subprocess.run(['systemctl', 'unmask', '--runtime', 'bluetooth.service'], check=False)
            subprocess.run(['systemctl', 'start', 'bluetooth.service'], check=False)
            raise
        if action == 'disable':
            backup.unlink()
    print('Watch-specific LE timing enabled: 45ms interval, zero latency, 5s supervision timeout.'
          if action == 'enable' else 'Previous watch connection parameters restored.')
    print('Pairing key sections were preserved. Reconnect the watch to validate the negotiated values.')

if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(f'Timing update failed: {type(error).__name__}: {error}', file=sys.stderr)
        sys.exit(1)
