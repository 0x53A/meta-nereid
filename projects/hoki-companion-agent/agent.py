#!/usr/bin/env python3
"""Hoki companion v1: one bounded JSON request on stdin, one JSON reply.

Invoked through authenticated SSH; no listener, shell interpolation or credential
arguments. Root is required for mutations. Lukas Rieger <code@lukasrieger.com>.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import tempfile
import time

VERSION = 1
LIMIT = 8192
CONNMAN = Path('/var/lib/connman')
ZONES = Path('/usr/share/zoneinfo')
BATTERY = Path('/sys/class/power_supply/battery')


class RequestError(Exception):
    pass


def run(*args):
    try:
        result = subprocess.run(args, stdin=subprocess.DEVNULL, capture_output=True,
                                text=True, timeout=8, check=True)
        return result.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        # Neither arguments nor subprocess output are returned to the phone/log.
        raise RequestError('Watch service unavailable or request timed out') from None


def bus(*args):
    return json.loads(run('busctl', '--system', '--timeout=6', '--json=short', *args))['data']


def prop(interface, path, name):
    return bus('get-property', interface, path, interface, name)


def text_file(path):
    try:
        return path.read_text().strip()
    except OSError:
        return None


def optional(call):
    try:
        return call()
    except (RequestError, ValueError, KeyError, TypeError):
        return None


def battery():
    raw = text_file(BATTERY / 'capacity')
    level = int(raw) if raw and raw.isdigit() and 0 <= int(raw) <= 100 else None
    state = text_file(BATTERY / 'status')
    charging = {'Charging': True, 'Full': True, 'Discharging': False,
                'Not charging': False}.get(state)
    return {'percent': level, 'charging': charging, 'state': state}


def networks():
    services = bus('call', 'net.connman', '/', 'net.connman.Manager', 'GetServices')[0]
    return [{'name': p.get('Name', {}).get('data', ''),
             'state': p.get('State', {}).get('data', 'unknown'),
             'type': p.get('Type', {}).get('data', 'unknown')}
            for _, p in services if p.get('Type', {}).get('data') in ('wifi', 'bluetooth', 'ethernet')]


def tailscale():
    result = json.loads(run('tailscale', 'status', '--json'))
    peer = result.get('Self') or {}
    return {'state': result.get('BackendState', 'unknown'),
            'online': peer.get('Online'), 'addresses': result.get('TailscaleIPs', [])}


def status():
    uptime = optional(lambda: float(Path('/proc/uptime').read_text().split()[0]))
    return {'protocol': VERSION, 'capabilities': ['status', 'sync_time', 'save_wifi'],
            'hostname': socket.gethostname(), 'battery': battery(),
            'utc_ms': int(time.time() * 1000), 'uptime_seconds': uptime,
            'timezone': optional(lambda: prop('org.freedesktop.timedate1', '/org/freedesktop/timedate1', 'Timezone')),
            'networks': optional(networks), 'tailscale': optional(tailscale),
            # This request proves local SSH access, not access from another tailnet peer.
            'remote_ssh': 'not_tested'}


def wifi_config(request):
    ssid = request.get('ssid')
    security = request.get('security')
    password = request.get('password', '')
    hidden = request.get('hidden', False)
    if not isinstance(ssid, str) or not 1 <= len(ssid.encode('utf-8')) <= 32 or '\0' in ssid:
        raise RequestError('SSID must contain 1–32 UTF-8 bytes, without NUL')
    if security not in ('open', 'wpa-psk') or not isinstance(password, str) or type(hidden) is not bool:
        raise RequestError('Unsupported Wi-Fi configuration')
    if security == 'open' and password:
        raise RequestError('An open network must not have a password')
    if security == 'wpa-psk':
        is_hex = re.fullmatch(r'[0-9a-fA-F]{64}', password)
        is_phrase = 8 <= len(password) <= 63 and all(32 <= ord(c) <= 126 for c in password)
        if not (is_hex or is_phrase):
            raise RequestError('Use 8–63 printable ASCII characters or a 64-digit hexadecimal WPA key')
    ssid_hex = ssid.encode('utf-8').hex()
    lines = ['[service_companion]', 'Type=wifi', 'SSID=' + ssid_hex]
    if security == 'wpa-psk':
        lines.append('Passphrase=' + password.replace('\\', '\\\\').replace(' ', '\\s'))
    lines.append('Hidden=' + str(hidden).lower())
    # Alphanumeric basename required by ConnMan. Include security to avoid ambiguity.
    name = 'hokicompanion' + hashlib.sha256((security + ':' + ssid_hex).encode()).hexdigest() + '.config'
    return name, ('\n'.join(lines) + '\n').encode()


def save_wifi(request, directory=CONNMAN):
    name, data = wifi_config(request)
    # Reject duplicates owned by image personalization/other provisioners: ConnMan
    # may otherwise select their old password instead of ours. Never overwrite them.
    import configparser
    ssid_hex = request['ssid'].encode().hex()
    for existing in directory.glob('*.config'):
        if existing.name == name:
            continue
        config = configparser.ConfigParser(interpolation=None, strict=False)
        try:
            config.read_string(existing.read_text())
        except (OSError, UnicodeError, configparser.Error):
            raise RequestError('Cannot check existing Wi-Fi provisioning') from None
        for section in config.sections():
            if config.get(section, 'SSID', fallback='').lower() == ssid_hex:
                raise RequestError('This network is already provisioned outside the companion; update its existing configuration first')
            if config.get(section, 'Name', fallback=None) == request['ssid']:
                raise RequestError('This network is already provisioned outside the companion; update its existing configuration first')
    # Directory is root-owned on the watch. Replacement never follows destination
    # symlinks. Temporary files are private and not recognized by ConnMan.
    fd, temporary = tempfile.mkstemp(prefix='.companion-', dir=directory)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, directory / name)
        parent = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return {'saved': True, 'connected': None,
            'message': 'Saved on watch. Connection has not been tested. Select the network in watch Settings.'}


def sync_time(request):
    received = time.monotonic()
    millis, zone = request.get('utc_ms'), request.get('timezone')
    if type(millis) is not int or not 1577836800000 <= millis < 4102444800000:
        raise RequestError('Phone time must be between 2020 and 2100')
    if not isinstance(zone, str) or not re.fullmatch(r'[A-Za-z0-9_+/-]{1,100}', zone):
        raise RequestError('Invalid timezone')
    candidate = (ZONES / zone).resolve()
    if not candidate.is_relative_to(ZONES.resolve()) or not candidate.is_file():
        raise RequestError('Timezone is not installed on the watch')
    if candidate.read_bytes()[:4] != b'TZif':
        raise RequestError('Invalid timezone data')
    # Do not disable NTP. Seed the realtime clock only while it is unsynchronized;
    # BOOTTIME/monotonic recording timestamps are unaffected by clock_settime.
    synced = prop('org.freedesktop.timedate1', '/org/freedesktop/timedate1', 'NTPSynchronized')
    run('busctl', '--system', '--timeout=6', 'call', 'org.freedesktop.timedate1',
        '/org/freedesktop/timedate1', 'org.freedesktop.timedate1', 'SetTimezone', 'sb', zone, 'false')
    adjusted = False
    if synced is False and abs(time.time() - millis / 1000) > 2:
        time.clock_settime(time.CLOCK_REALTIME, millis / 1000 + time.monotonic() - received)
        adjusted = True
    return {'clock_adjusted': adjusted, 'timezone': zone,
            'message': 'Timezone synced; watch already synchronized by NTP' if synced else 'Phone time and timezone synced'}


def handle(request):
    if not isinstance(request, dict) or type(request.get('version')) is not int or request['version'] != VERSION:
        raise RequestError('Unsupported companion protocol')
    op = request.get('op')
    allowed = {'status': {'version', 'op'}, 'save_wifi': {'version', 'op', 'ssid', 'password', 'security', 'hidden'},
               'sync_time': {'version', 'op', 'utc_ms', 'timezone'}}
    if op not in allowed or set(request) - allowed[op]:
        raise RequestError('Unsupported request')
    if op == 'status':
        return status()
    if os.geteuid() != 0:
        raise RequestError('Watch administrator access is required')
    return save_wifi(request) if op == 'save_wifi' else sync_time(request)


def main():
    try:
        raw = sys.stdin.buffer.readline(LIMIT + 1)
        if len(raw) > LIMIT or not raw.endswith(b'\n'):
            raise RequestError('Request too large or incomplete')
        response = {'ok': True, 'result': handle(json.loads(raw))}
    except RequestError as error:
        response = {'ok': False, 'error': str(error)}
    except (ValueError, TypeError, OSError, KeyError):
        response = {'ok': False, 'error': 'Watch request failed; no operation was confirmed'}
    print(json.dumps(response, ensure_ascii=True), flush=True)


if __name__ == '__main__':
    main()
