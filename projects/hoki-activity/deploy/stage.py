#!/usr/bin/env python3
"""Stage the coordinated local PoC payload; never connects to or changes a watch."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil


def stage(destination, activity, recorder, spo2):
    project = Path(__file__).resolve().parents[1]
    health = project.parent / 'hoki-health-recorder/deploy'
    entries = [
        (activity, 'usr/lib/hoki-activity', 0o755),
        (recorder, 'usr/bin/hoki-health-recorder', 0o755),
        (spo2, 'usr/lib/hoki-spo2', 0o755),
        (project/'deploy/hoki-activity', 'usr/bin/hoki-activity', 0o755),
        (project/'deploy/hoki-activity.desktop', 'usr/share/applications/hoki-activity.desktop', 0o644),
        (project/'deploy/hoki-activity.service', 'usr/lib/systemd/user/hoki-activity.service', 0o644),
        (project/'daemon.py', 'usr/libexec/hoki-activity/daemon.py', 0o755),
        (project/'activity.py', 'usr/libexec/hoki-activity/activity.py', 0o644),
        (project/'README.md', 'usr/share/hoki-activity/README.md', 0o644),
        (project/'export.py', 'usr/share/hoki-activity/export.py', 0o644),
    ]
    for name in ('health_client.py', 'power_client.py'):
        entries += [(health/name, 'usr/libexec/hoki-activity/'+name, 0o644),
                    (health/name, 'usr/libexec/'+name, 0o644)]
    for source, target in [('health-policy.py','hoki-health-policy'), ('manual-consumer.py','hoki-manual-consumer')]:
        entries.append((health/source, 'usr/libexec/'+target, 0o755))
    entries.append((health/'health_broker.py','usr/libexec/health_broker.py',0o644))
    for unit in ('hoki-health-recording.service', 'hoki-health-policy.service', 'hoki-health-profile-recording.service'):
        entries.append((health/unit, 'usr/lib/systemd/system/'+unit, 0o644))
    for binary in (activity, recorder, spo2):
        data = binary.read_bytes()
        if data[:6] != b'\x7fELF\x01\x01' or data[18:20] != b'\x28\x00':
            raise ValueError('Expected ARM32 ELF: '+str(binary))
    destination.mkdir(parents=True, exist_ok=False)
    hashes = {}
    for source, target, mode in entries:
        output = destination / target
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, output)
        output.chmod(mode)
        hashes[target] = hashlib.sha256(output.read_bytes()).hexdigest()
    (destination/'manifest.json').write_text(json.dumps(dict(version=1, files=hashes), indent=2)+'\n')
    return len(hashes)

if __name__ == '__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for argument in ('destination','activity','recorder','spo2'):
        p.add_argument(argument,type=Path)
    args=p.parse_args()
    print('Staged',stage(args.destination,args.activity,args.recorder,args.spo2),'files')
