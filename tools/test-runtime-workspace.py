#!/usr/bin/env python3
"""Run ARM workspace tests using a completed BitBake recipe's environment.

Run inside the build container after `bitbake asteroid-image qemu-native`. The host
container needs dbus-daemon for the private-bus tests. No watch access is used.
"""
import argparse
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('build_dir', type=Path)
    args = parser.parse_args()
    build = args.build_dir.resolve()
    candidates = list(build.glob('tmp/work/hoki-*/hoki-ui/*/temp/run.do_compile'))
    if len(candidates) != 1:
        raise SystemExit(f'Expected one completed hoki-ui recipe environment, found {len(candidates)}')
    run = candidates[0]
    qemu = build / 'tmp/sysroots-components/x86_64/qemu-native/usr/bin/qemu-arm'
    if not qemu.is_file() or not shutil.which('dbus-daemon'):
        raise SystemExit('Build qemu-native and install host dbus-daemon first')
    script = run.read_text()
    pattern = r'cargo build -v --frozen --release(?P<sbom> -Z sbom)? --target (?P<target>\S+)'
    matches = list(re.finditer(pattern, script))
    if len(matches) != 1 or script.count('\ndo_compile\n') != 1:
        raise SystemExit('Unexpected BitBake compile script; refusing to rewrite it')
    target = matches[0]['target']
    script = re.sub(pattern, r'cargo test --no-fail-fast -v --frozen --release\g<sbom> --target \g<target>', script)
    if script.count('--workspace --bins "$@"') != 1:
        raise SystemExit('Unexpected workspace selection in compile script')
    script = script.replace('--workspace --bins "$@"', '--workspace "$@" -- --test-threads=1')
    images = list(build.glob('tmp/work/hoki-*/asteroid-image/*/rootfs'))
    if len(images) != 1:
        raise SystemExit('Build asteroid-image first for runtime plugins and XKB data')
    rootfs = images[0]
    runner = shlex.join([str(qemu), '-L', str(rootfs)])
    setup = (f'export CARGO_TARGET_{target.upper().replace("-", "_")}_RUNNER={shlex.quote(runner)}\n'
             f'export HOKI_CONNECT_TEST_QEMU={shlex.quote(str(qemu))}\n'
             f'export QEMU_LD_PREFIX={shlex.quote(str(rootfs))}\n'
             f'export XKB_CONFIG_ROOT={shlex.quote(str(rootfs / "usr/share/X11/xkb"))}\n'
             f'export GST_PLUGIN_SYSTEM_PATH_1_0={shlex.quote(str(rootfs / "usr/lib/gstreamer-1.0"))}\n'
             'export GST_REGISTRY_FORK=no\n'
             'export CARGO_BUILD_JOBS=4\n'
             'export PATH="$PATH:/usr/bin:/bin"\n')
    script = script.replace('\ndo_compile\n', '\n' + setup + 'do_compile\n')
    with tempfile.TemporaryDirectory(prefix='workspace-tests-', dir=run.parent) as tmp:
        path = Path(tmp) / 'run-tests.sh'
        script = script.replace('\ndo_compile\n', '\nexport GST_REGISTRY_1_0=' + shlex.quote(str(Path(tmp) / 'gst-registry.bin')) + '\ndo_compile\n')
        path.write_text(script)
        subprocess.run(['/bin/sh', str(path)], check=True, timeout=1800)


if __name__ == '__main__':
    main()
