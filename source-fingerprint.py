#!/usr/bin/env python3
"""Hash payload inputs; used to reject stale hand-built runtime bundles."""
import hashlib
from pathlib import Path
root = Path(__file__).resolve().parent.parent
manifest = root / 'meta-nereid/runtime-projects.txt'
projects = [line.split('|')[0] for line in manifest.read_text().splitlines()
            if line and not line.startswith('#')]
projects.append('hoki-wasm-guest')
files = set()
for project in projects:
    base = root / project
    for name in ['Cargo.toml', 'Cargo.lock', 'build.rs', 'shell.nix', 'src', 'ui', 'deploy', 'opk', 'assets', 'fonts', '.cargo', 'cross-pc', 'cross-lib']:
        path = base / name
        if path.is_dir():
            files.update(p for p in path.rglob('*') if p.is_file()
                         and '__pycache__' not in p.parts
                         and not (project == 'hoki-audiobook' and name == 'cross-lib'
                                  and (p.name.endswith('.so') or 'result' in p.relative_to(path).parts)))
        elif path.is_file():
            files.add(path)
files.update(p for p in (root / 'shared').rglob('*') if p.is_file())
files.add(root / 'meta-nereid/build-runtime.sh')
files.add(root / 'meta-nereid/host-linker.sh')
files.add(root / 'meta-nereid/patch-watch-elf.sh')
files.add(root / 'meta-nereid/publish-runtime-archive.sh')
files.add(root / 'meta-nereid/check-runtime.py')
files.add(manifest)
files.add(Path(__file__).resolve())
hash_ = hashlib.sha256()
for path in sorted(files):
    hash_.update(str(path.relative_to(root)).encode() + b'\0')
    hash_.update(path.read_bytes())
print(hash_.hexdigest())
