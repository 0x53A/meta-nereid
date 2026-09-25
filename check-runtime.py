#!/usr/bin/env python3
"""Validate the app inventory and a freshly built Hoki runtime archive."""
import configparser
import hashlib
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile

def require(condition, message):
    if not condition:
        raise ValueError(message)


root = Path(__file__).resolve().parent
manifest = root / 'runtime-projects.txt'
projects = [line.split('|') for line in manifest.read_text().splitlines()
            if line and not line.startswith('#')]
core = {'nereid-compositor', 'hoki-hwc-proxy', 'hoki-launcher', 'hoki-settings',
        'hoki-powerd', 'hoki-radiod', 'hoki-connect', 'hoki-lp-watchface',
        'hoki-suspend-check'}
require(len({row[1] for row in projects}) == len(projects), 'Duplicate binary')
apps = {binary for _, binary, _ in projects if binary not in core | {'hoki-watchface'}}
recipe = (root / 'recipes-hoki/hoki-ui/hoki-apps.inc').read_text()
packaged = set(re.search(r'^HOKI_APP_PACKAGES = "([^"]+)"', recipe, re.M)[1].split())
group = (root / 'recipes-hoki/packagegroups/packagegroup-hoki-apps.bb').read_text()
selected = set(re.search(r'RDEPENDS:\$\{PN\} = "([^"]+)"', group)[1].replace('\\\n', ' ').split())
require(apps == packaged == selected, ('App inventory/package group mismatch', apps, packaged, selected))
for source, binary, dest in projects:
    base = root / "projects" / source
    for name in ('Cargo.toml', 'Cargo.lock', 'shell.nix'):
        require((base / name).is_file(), (source, name))
    require((base / 'src/main.rs').is_file(), source)
    if binary not in core:
        desktop = configparser.ConfigParser(interpolation=None)
        desktop.read(base / 'deploy' / (binary + '.desktop'))
        require(desktop['Desktop Entry']['Exec'] == binary, binary)
        wrapper = base / 'deploy' / binary
        if not wrapper.exists():
            wrapper = wrapper.with_suffix('.sh')
        contents = wrapper.read_text()
        require(contents.startswith('#!/bin/sh\n'), wrapper)
        require('XDG_RUNTIME_DIR=/run/user/1000' in contents, wrapper)
        require('WAYLAND_DISPLAY=wayland-0' in contents, wrapper)
        require('exec invoker --type=generic /usr/lib/' + binary in contents, wrapper)
print(f'PASS: {len(projects)} runtime projects, {len(apps)} app packages; every app has a matching launcher')
if len(sys.argv) == 1:
    sys.exit(0)
with tarfile.open(sys.argv[1]) as archive, tempfile.TemporaryDirectory() as tmp:
    def read(path):
        return archive.extractfile('hoki-runtime/' + path).read()

    def matches_source(path, source, executable=False):
        member = archive.getmember('hoki-runtime/' + path)
        require(member.isfile(), ('Packaged file is not regular', path))
        require(read(path) == source.read_bytes(), ('Packaged file differs from source', path))
        if executable:
            require(member.mode & 0o111, path)

    # These copied inputs are not in the binary-only checksum manifest. A source
    # fingerprint alone does not prove that their packaged bytes are current.
    for source, binary, _ in projects:
        deploy = root / "projects" / source / 'deploy'
        desktop = deploy / (binary + '.desktop')
        if desktop.is_file():
            wrapper = deploy / binary
            if not wrapper.is_file():
                wrapper = wrapper.with_suffix('.sh')
            matches_source('usr/bin/' + binary, wrapper, executable=True)
            matches_source('usr/share/applications/' + binary + '.desktop', desktop)
    for project in ('hoki-powerd', 'hoki-radiod'):
        deploy = root / "projects" / project / 'deploy'
        matches_source('usr/lib/systemd/system/' + project + '.service', deploy / (project + '.service'))
        for pattern, destination in (('org.hoki.*.conf', 'etc/dbus-1/system.d/'),
                                     ('org.hoki.*.service', 'usr/share/dbus-1/system-services/')):
            sources = sorted(deploy.glob(pattern))
            require(sources, (project, pattern))
            for source in sources:
                matches_source(destination + source.name, source)
    matches_source('usr/lib/systemd/system/hoki-rsb-enable.service',
                   root / 'projects/nereid-compositor/opk/hoki-rsb-enable.service')
    matches_source('usr/lib/systemd/user/hoki-connect.service',
                   root / 'projects/hoki-connect/deploy/hoki-connect.service')
    matches_source('usr/lib/systemd/user/hoki-music.service',
                   root / 'projects/hoki-music/deploy/hoki-music.service')

    for source, binary, dest in projects:
        path = dest + '/' + binary
        member = archive.getmember('hoki-runtime/' + path)
        require(member.mode & 0o111, path)
        data = read(path)
        # ELF32 little-endian ARM, not an accidentally copied host binary.
        require(data[:6] == b'\x7fELF\x01\x01' and data[18:20] == b'\x28\x00', path)
        elf = Path(tmp) / binary
        elf.write_bytes(data)
        headers = subprocess.check_output(['readelf', '-l', str(elf)], text=True)
        dynamic = subprocess.check_output(['readelf', '-d', str(elf)], text=True)
        interpreters = re.findall(r'\[Requesting program interpreter: ([^\]]*)\]', headers)
        require(interpreters == ['/lib/ld-linux-armhf.so.3'], ('incorrect runtime interpreter', path))
        runpaths = re.findall(r'\((?:RPATH|RUNPATH)\).*?\[(.*?)\]', dynamic)
        require(runpaths == ['/usr/lib:/lib'] and '/nix/store/' not in dynamic,
                ('incorrect runtime library path', path))
        if binary not in core:
            for app_path in ('usr/bin/' + binary, 'usr/share/applications/' + binary + '.desktop'):
                require(read(app_path), app_path)
            require(archive.getmember('hoki-runtime/usr/bin/' + binary).mode & 0o111, 'Runtime validation failed')
    checksum_lines = read('usr/share/hoki/runtime-sha256.txt').decode().splitlines()
    expected_paths = {dest + '/' + binary for _, binary, dest in projects}
    actual_paths = set()
    for line in checksum_lines:
        checksum, path = line.split('  ', 1)
        require(hashlib.sha256(read(path)).hexdigest() == checksum, path)
        actual_paths.add(path)
    require(actual_paths == expected_paths, (actual_paths, expected_paths))
    fingerprint = subprocess.check_output([sys.executable, str(root / 'source-fingerprint.py')])
    require(read('usr/share/hoki/runtime-source.sha256') == fingerprint, 'Stale payload')
print('PASS: ARM binaries, loaders, library paths, deployment files, hashes and source fingerprint')
