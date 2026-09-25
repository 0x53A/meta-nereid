#!/usr/bin/env python3
"""Upload a rootfs bundle over SSH, select a trial, optionally reboot and confirm."""
import argparse
import json
from pathlib import Path
import re
import shlex
import subprocess
import time


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('bundle', type=Path)
    p.add_argument('--host', required=True, help='SSH destination')
    p.add_argument('--host-key-alias', help='Verify an existing identity when connecting by another address')
    p.add_argument('--reboot', action='store_true', help='Clean reboot, wait for SSH and check services before confirmation')
    p.add_argument('--timeout', type=int, default=180)
    a = p.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9_.@:-]+', a.host) or a.host.startswith('-'):
        p.error('Invalid SSH host')
    data = json.loads((a.bundle / 'manifest.json').read_text())
    v = data['version']
    if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_-]{0,63}', v) or v == 'legacy':
        p.error('Invalid version')
    ssh = ['ssh', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
           '-o', 'ConnectTimeout=5', '-o', 'ServerAliveInterval=5',
           '-o', 'ServerAliveCountMax=2']
    if a.host_key_alias:
        ssh += ['-o', 'HostKeyAlias=' + a.host_key_alias]

    def remote(command, check=True, timeout=60):
        return subprocess.run(ssh + [a.host, command], check=check, capture_output=True, text=True, timeout=timeout)

    # No legacy direct-root updates: the managed store must already be mounted.
    remote('mountpoint -q /userdata && test -f /userdata/.hoki/selection')
    remote('test ! -e /userdata/.hoki/versions/' + v)
    incoming = '/userdata/.hoki/incoming/' + v
    remote('install -d -m0700 ' + incoming)
    available = int(remote("df -Pk /userdata | awk 'NR == 2 {print $4}'").stdout.strip()) * 1024
    uploaded = int(remote("du -sk " + incoming + " | awk '{print $1}'").stdout.strip()) * 1024
    required = data['rootfs_size'] + data['recovery_size'] + 128 * 1024 * 1024
    if available + uploaded < required:
        raise SystemExit('Insufficient userdata space for bundle plus 128 MiB reserve.')
    subprocess.run(['rsync', '-rt', '--partial', '--protect-args', '-e', shlex.join(ssh),
                    str(a.bundle.resolve()) + '/', a.host + ':' + incoming + '/'], check=True)
    remote('hoki-rootfs stage ' + incoming, timeout=600)
    remote('hoki-rootfs activate ' + v)
    print('Uploaded, verified and selected trial ' + v, flush=True)
    if not a.reboot:
        print('Reboot normally, validate, then run hoki-rootfs confirm.')
        return
    old_boot = remote('cat /proc/sys/kernel/random/boot_id').stdout.strip()
    # systemctl can close SSH while completing a successful reboot request.
    remote('systemctl reboot', check=False)
    deadline = time.monotonic() + a.timeout
    healthy_since = None
    observed_boot = None
    while time.monotonic() < deadline:
        time.sleep(3)
        try:
            result = remote('cat /proc/sys/kernel/random/boot_id; cat /etc/hoki-rootfs-booted', check=False)
            fields = result.stdout.splitlines()
            if result.returncode or len(fields) != 2 or fields[0] == old_boot:
                healthy_since = None
                continue
            if fields[0] != observed_boot:
                observed_boot = fields[0]
                healthy_since = None
            if fields[1] != v:
                raise SystemExit('Watch booted a different version; trial was not confirmed.')
            # SSH is already proven by this authenticated connection; the
            # image uses sshd.socket, so sshd.service need not be active.
            health = remote('for unit in connman hoki-powerd hoki-radiod; do '
                            'systemctl is-active --quiet "$unit" || exit 1; done; '
                            'systemctl --user -M ceres@ is-active --quiet nereid-compositor', check=False)
            if health.returncode:
                healthy_since = None
                continue
            if healthy_since is None:
                healthy_since = time.monotonic()
            if time.monotonic() - healthy_since < 15:
                continue
            remote('hoki-rootfs confirm')
            print('New boot, SSH and core services verified; version confirmed.')
            print('Hardware audio/sensor validation is separate. Previous images retained.')
            return
        except (subprocess.TimeoutExpired, subprocess.CalledProcessError):
            healthy_since = None
            continue
    raise SystemExit('Confirmation timed out; trial remains unconfirmed. A subsequent reboot selects the prior version.')


if __name__ == '__main__':
    main()
