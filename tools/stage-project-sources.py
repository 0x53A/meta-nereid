#!/usr/bin/env python3
"""Stage only declared build inputs, never project caches or private data links."""
import argparse
from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parents[1]


def inputs():
    paths = set()
    for name in ('recipes-hoki/hoki-ui/hoki-sources.inc',
                 'recipes-hoki/hoki-health-recorder/health-sources.inc',
                 'recipes-connectivity/ble-ssh/ble-sources.inc',
                 'recipes-hoki/hoki-health-recorder/hoki-health-recorder_0.1.0.bb',
                 'recipes-connectivity/ble-ssh/ble-ssh-watch_0.1.0.bb',
                 'recipes-devtools/hoki-wasm/hoki-wasm-guest-native_1.90.0.bb'):
        source_uris = '\n'.join(re.findall(r'SRC_URI\s*\+?=\s*"([^"]*)"', (ROOT / name).read_text()))
        for path in re.findall(r'file://([^;\s"\\]+)', source_uris):
            source = ROOT / 'projects' / path
            if not source.exists():
                raise ValueError(f'Missing declared source input: {source}')
            paths.add(source)
    for project in ('nfcd-linux-plugin', 'hoki-nfc-test-card', 'hoki-gps-recorder'):
        for name in ('src', 'Makefile', 'LICENSE', 'README.md', 'geocluerecorder.cpp', 'geocluerecorder.h', 'main.qml'):
            path = ROOT / 'projects' / project / name
            if path.exists():
                paths.add(path)
    return paths


def stage(destination):
    destination = Path(destination)
    if destination.exists() and any(destination.iterdir()):
        raise ValueError('Source staging destination must be empty')
    destination.mkdir(parents=True, exist_ok=True)
    for path in sorted(inputs()):
        if path.is_symlink():
            raise ValueError(f'Symlink is not a self-contained source input: {path}')
        files = path.rglob('*') if path.is_dir() else [path]
        for source in files:
            if source.is_symlink():
                raise ValueError(f'Symlink is not a self-contained source input: {source}')
            if not source.is_file() or '__pycache__' in source.parts:
                continue
            output = destination / source.relative_to(ROOT / 'projects')
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, output)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('destination')
    stage(parser.parse_args().destination)
