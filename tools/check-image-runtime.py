#!/usr/bin/env python3
"""Read-only checks of source-built runtime executables and launchers in ext4."""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def extract(image, path, output):
    output.unlink(missing_ok=True)
    result = subprocess.run(['debugfs', '-R', f'dump {path} {output}', str(image)],
                            text=True, capture_output=True)
    if result.returncode or not output.is_file():
        raise RuntimeError(f'Missing image file {path}: {result.stderr}')


def check(image, with_wasm_demo=False):
    entries = [line.split('|') for line in (ROOT / 'runtime-projects.txt').read_text().splitlines()
               if line and not line.startswith('#')]
    if not with_wasm_demo:
        entries = [entry for entry in entries if entry[0] != 'hoki-wasm-host']
    with tempfile.TemporaryDirectory(prefix='hoki-image-check-') as tmp:
        tmp = Path(tmp)
        if not with_wasm_demo:
            for path in ('/usr/lib/hoki-wasm-host', '/usr/bin/hoki-wasm-host',
                         '/usr/share/applications/hoki-wasm-host.desktop'):
                output = tmp / 'disabled-wasm'
                try:
                    extract(image, path, output)
                except RuntimeError:
                    pass
                else:
                    raise RuntimeError(f'Disabled WASM demo is present: {path}')
        binaries = [(f'/{dest}/{binary}', '/usr/lib/ld-linux-armhf.so.3') for _, binary, dest in entries]
        binaries += [('/usr/bin/ble-ssh-watch', '/usr/lib/ld-linux-armhf.so.3'),
                     ('/usr/bin/hoki-health-recorder', '/usr/lib/ld-linux-armhf.so.3'),
                     ('/usr/libexec/hoki-ssc-recorder', '/system/bin/linker')]
        for index, loader in enumerate(sorted({loader for _, loader in binaries})):
            output = tmp / f'loader-{index}'
            extract(image, loader, output)
            headers = subprocess.check_output(['readelf', '-h', str(output)], text=True)
            if not re.search(r'Machine:\s+ARM\b', headers):
                raise RuntimeError(f'{loader}: not an ARM loader')
        for index, (path, interpreter) in enumerate(binaries):
            output = tmp / f'binary-{index}'
            extract(image, path, output)
            result = subprocess.run(['readelf', '-h', '-l', '-d', str(output)],
                                    check=True, text=True, capture_output=True).stdout
            if not re.search(r'Machine:\s+ARM\b', result):
                raise RuntimeError(f'{path}: not ARM')
            if f'Requesting program interpreter: {interpreter}' not in result:
                raise RuntimeError(f'{path}: unexpected ELF interpreter')
            for line in result.splitlines():
                if any(tag in line for tag in ('(RPATH)', '(RUNPATH)', '(NEEDED)')):
                    if '/nix/' in line or '/asteroid/' in line or '/home/' in line:
                        raise RuntimeError(f'{path}: build-host library path: {line}')
        applications = 0
        for project, binary, _ in entries:
            desktop = ROOT / 'projects' / project / 'deploy' / (binary + '.desktop')
            if not desktop.exists():
                continue
            output = tmp / 'desktop'
            extract(image, f'/usr/share/applications/{binary}.desktop', output)
            text = output.read_text()
            if not re.search(rf'^Exec={re.escape(binary)}(?:\s|$)', text, re.M):
                raise RuntimeError(f'{binary}: wrong desktop launcher')
            output = tmp / 'launcher'
            output.unlink(missing_ok=True)
            extract(image, f'/usr/bin/{binary}', output)
            if not output.read_text().startswith('#!'):
                raise RuntimeError(f'{binary}: missing launcher script')
            applications += 1
        print(f'PASS: {len(binaries)} ARM runtime binaries and {applications} desktop launchers in {image}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('image', type=Path)
    parser.add_argument('--with-wasm-demo', action='store_true',
                        help='Validate an image built with HOKI_WASM_DEMO = "1"')
    args = parser.parse_args()
    check(args.image.resolve(), args.with_wasm_demo)
