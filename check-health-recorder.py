#!/usr/bin/env python3
"""Check a prepared health archive against local sources without executing it."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent
PREFIX = 'health-recorder-runtime/'
EXECUTABLES = {'usr/bin/hoki-health-recorder', 'usr/libexec/hoki-ssc-recorder',
               'usr/libexec/hoki-recording-suspend-loop'}
FILES = EXECUTABLES | {'usr/share/hoki-health-recorder/source.sha256',
                       'usr/share/hoki-health-recorder/binaries.sha256',
                       'usr/share/hoki-health-recorder/README.md',
                       'usr/share/hoki-health-recorder/CAPABILITIES.md'}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def verify(archive_path):
    expected = subprocess.check_output([sys.executable, str(ROOT/'meta-nereid/health-recorder-fingerprint.py')])
    with tarfile.open(archive_path) as archive, tempfile.TemporaryDirectory() as directory:
        members = archive.getmembers()
        require(len({m.name for m in members}) == len(members), 'duplicate archive member')
        files = {PREFIX+name for name in FILES}
        directories = {str(parent) for name in files for parent in Path(name).parents
                       if str(parent) != '.'}
        for member in members:
            require((member.name in files and member.isfile())
                    or (member.name in directories and member.isdir()),
                    f'unexpected archive member or type: {member.name}')

        def read(path):
            member = archive.getmember(PREFIX+path)
            require(member.isfile(), f'not a regular file: {path}')
            return archive.extractfile(member).read()

        require(read('usr/share/hoki-health-recorder/source.sha256') == expected,
                'health source fingerprint mismatch')
        checksums = read('usr/share/hoki-health-recorder/binaries.sha256').decode().splitlines()
        paths = set()
        for line in checksums:
            parts = line.split('  ', 1)
            require(len(parts) == 2, 'malformed checksum entry')
            digest, path = parts
            require(re.fullmatch('[0-9a-f]{64}', digest) is not None, 'invalid checksum')
            require(path in EXECUTABLES and path not in paths, f'unexpected or duplicate executable: {path}')
            paths.add(path)
            require(hashlib.sha256(read(path)).hexdigest() == digest, f'checksum mismatch: {path}')
            require(archive.getmember(PREFIX+path).mode & 0o111, f'not executable: {path}')
        require(paths == EXECUTABLES, 'missing executable checksum')
        for path, interpreter in [('usr/bin/hoki-health-recorder', '/lib/ld-linux-armhf.so.3'),
                                  ('usr/libexec/hoki-ssc-recorder', '/system/bin/linker')]:
            data = read(path)
            require(data[:6] == b'\x7fELF\x01\x01' and data[18:20] == b'\x28\x00',
                    f'not little-endian ELF32 ARM: {path}')
            elf = Path(directory)/Path(path).name
            elf.write_bytes(data)
            headers = subprocess.check_output(['readelf', '-l', str(elf)], text=True)
            require(f'Requesting program interpreter: {interpreter}]' in headers,
                    f'incorrect interpreter: {path}')
            dynamic = subprocess.check_output(['readelf', '-d', str(elf)], text=True)
            require('/nix/store/' not in dynamic, f'Nix runtime path: {path}')
            if path == 'usr/bin/hoki-health-recorder':
                runpaths = re.findall(r'\((?:RPATH|RUNPATH)\).*?\[(.*?)\]', dynamic)
                require(runpaths == ['/usr/lib:/lib'], f'incorrect library path: {path}')
        for packaged, source in [('usr/libexec/hoki-recording-suspend-loop', 'deploy/suspend-loop.sh'),
                                 ('usr/share/hoki-health-recorder/README.md', 'README.md'),
                                 ('usr/share/hoki-health-recorder/CAPABILITIES.md', 'CAPABILITIES.md')]:
            require(read(packaged) == (ROOT/'hoki-health-recorder'/source).read_bytes(),
                    f'packaged source mismatch: {packaged}')
    return dict(source_fingerprint_matches=True, packaged_hashes_match=True,
                arm_architecture_and_interpreters_verified=True, packaged_scripts_and_docs_match=True,
                archive_sha256=hashlib.sha256(Path(archive_path).read_bytes()).hexdigest())


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', nargs='?', type=Path, default=ROOT/'meta-nereid/recipes-hoki/hoki-health-recorder/files/health-recorder-runtime.tar.gz')
    args = parser.parse_args()
    try:
        print(json.dumps(verify(args.archive)))
    except (ValueError, KeyError, OSError, tarfile.TarError, subprocess.SubprocessError) as error:
        parser.exit(1, f'Health bundle verification failed: {error}\n')
