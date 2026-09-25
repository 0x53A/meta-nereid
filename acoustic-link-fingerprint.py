#!/usr/bin/env python3
"""Fingerprint the source inputs of the acoustic daemon runtime archive."""
import hashlib
from pathlib import Path
root = Path(__file__).resolve().parent.parent
project = root / 'acoustic-link'
files = [project / name for name in ('Cargo.toml', 'Cargo.lock', 'shell.nix',
         'rust-toolchain.toml', 'ofdm-profile.json', 'training-plan.csv', 'training-frequency-plan.csv')]
files += list((project / 'src').rglob('*.rs'))
files += list((project / 'deploy').glob('*.service'))
files += [root / 'meta-nereid' / name for name in
          ('build-acoustic-link.sh', 'acoustic-link-fingerprint.py', 'host-linker.sh')]
files += [root / 'shared/acoustic_volume.rs']
digest = hashlib.sha256()
for path in sorted(files):
    digest.update(str(path.relative_to(root)).encode() + b'\0')
    digest.update(path.read_bytes())
print(digest.hexdigest())
