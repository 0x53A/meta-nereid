#!/usr/bin/env python3
"""Association-preserving Prima suspend preparation, owned by systemd-suspend.

ANY prevents cfg80211 station teardown; it does not program selective firmware
wake filters. Preserve existing triggers. Restore after failed entry as well as
resume. The marker lives outside powerd's RuntimeDirectory so daemon restarts
cannot erase outstanding cleanup. A failed cleanup blocks the next preparation.
"""
import array
import fcntl
import json
from pathlib import Path
import socket
import subprocess
import struct
import sys

STATE = Path('/run/hoki-wifi-sleep/state.json')
INTERFACE = Path('/sys/class/net/wlan0')


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True,
                          timeout=8).stdout


def quiet(enabled):
    # Reviewed hdd_priv_data_t / SIOCDEVPRIVATE+1 ABI, shared with the existing
    # hoki-wifi-quiet diagnostic. Run in a bounded child, not inside powerd.
    command = array.array('B', b'SETSUSPENDMODE ' + (b'1' if enabled else b'0') + b'\0')
    data = array.array('B', struct.pack('@Pii', command.buffer_info()[0], 0, len(command)))
    request = bytearray(struct.pack('@16sP', b'wlan0', data.buffer_info()[0]))
    request.extend(bytes((40 if struct.calcsize('P') == 8 else 32) - len(request)))
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        fcntl.ioctl(sock.fileno(), 0x89f1, request, True)


def driver(mode):
    run(sys.executable, str(Path(__file__).resolve()), mode)


def phy():
    name = (INTERFACE / 'phy80211').resolve(strict=True).name
    if not name.startswith('phy') or not name[3:].isdigit():
        raise RuntimeError('invalid WLAN phy')
    return name


def disabled(output):
    if output.strip() == 'WoWLAN is disabled.':
        return True
    if output.startswith('WoWLAN is enabled:'):
        return False
    raise RuntimeError('unrecognized WoWLAN status: ' + output)


def restore():
    if not STATE.exists():
        return
    state = json.loads(STATE.read_text())
    # Do not apply an old transaction to a replaced interface/phy.
    if phy() != state['phy'] or int((INTERFACE / 'ifindex').read_text()) != state['ifindex']:
        raise RuntimeError('WLAN identity changed; outstanding cleanup retained')
    errors = []
    try:
        driver('resume')
    except Exception as error:
        errors.append(str(error))
    if state['restore_disabled']:
        try:
            run('/usr/sbin/iw', 'phy', state['phy'], 'wowlan', 'disable')
        except Exception as error:
            errors.append(str(error))
    if errors:
        raise RuntimeError('; '.join(errors))
    STATE.unlink()


def prepare():
    restore()
    try:
        flags = int((INTERFACE / 'flags').read_text().strip(), 16)
    except FileNotFoundError:
        if INTERFACE.exists():
            raise
        return
    if not flags & 1:
        return
    name = phy()
    was_disabled = disabled(run('/usr/sbin/iw', 'phy', name, 'wowlan', 'show'))
    state = dict(phy=name, ifindex=int((INTERFACE / 'ifindex').read_text()),
                 restore_disabled=was_disabled)
    STATE.parent.mkdir(mode=0o700, exist_ok=True)
    pending = STATE.with_suffix('.pending')
    pending.write_text(json.dumps(state))
    pending.replace(STATE)
    try:
        if was_disabled:
            run('/usr/sbin/iw', 'phy', name, 'wowlan', 'enable', 'any')
        if disabled(run('/usr/sbin/iw', 'phy', name, 'wowlan', 'show')):
            raise RuntimeError('WoWLAN preparation did not persist')
        driver('quiet')
    except Exception:
        restore()
        raise


if __name__ == '__main__':
    try:
        mode = sys.argv[1]
        if mode == 'prepare':
            prepare()
        elif mode == 'restore':
            restore()
        elif mode in ('quiet', 'resume'):
            quiet(mode == 'quiet')
        else:
            raise ValueError('expected prepare|restore|quiet|resume')
    except Exception as error:
        print(f'Wi-Fi sleep: {error}', file=sys.stderr)
        sys.exit(1)
