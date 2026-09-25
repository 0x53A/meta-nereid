#!/usr/bin/env python3
"""Fingerprint source inputs to the separately built health-recorder payload."""
import hashlib
from pathlib import Path

root = Path(__file__).resolve().parent
project = root / 'projects/hoki-health-recorder'
files = [project / name for name in ('Cargo.toml', 'Cargo.lock', 'shell.nix',
         'rust-toolchain.toml', 'README.md', 'CAPABILITIES.md', 'deploy/suspend-loop.sh',
         'deploy/recording-session.py', 'deploy/hoki-health-recording.service',
         'deploy/30-hoki-health-recording.rules',
         'ssc/build.sh')]
files.extend((project/'src').glob('*.rs'))
files.extend((project/'ssc').glob('*.c'))
files.extend((project/'ssc').glob('*.h'))
files.extend(root/name for name in
             ('health-recorder-fingerprint.py', 'build-health-recorder.sh', 'patch-watch-elf.sh',
              'check-health-recorder.py',
              'publish-runtime-archive.sh',
              'recipes-hoki/hoki-health-recorder/hoki-health-recorder_0.1.0.bb'))
digest = hashlib.sha256()
for path in sorted(files):
    digest.update(str(path.relative_to(root)).encode() + b'\0')
    digest.update(path.read_bytes())
print(digest.hexdigest())
