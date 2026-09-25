#!/usr/bin/env python3
"""Build ARM link-only stubs from the preserved export inventories."""
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def main():
    manifest_path, symbols_path, compiler, output_path = sys.argv[1:]
    manifest = json.loads(Path(manifest_path).read_text())
    output = Path(output_path)
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="hoki-link-stubs-") as temporary:
        for library, metadata in sorted(manifest.items()):
            symbols = (Path(symbols_path) / (library + ".txt")).read_text().splitlines()
            if len(set(symbols)) != metadata["symbols"] or len(symbols) != len(set(symbols)):
                raise ValueError(f"Invalid symbol count: {library}")
            if any(not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", symbol) for symbol in symbols):
                raise ValueError(f"Invalid symbol name: {library}")
            source = Path(temporary) / (library + ".c")
            source.write_text("\n".join(
                f'void stub_{i}(void) __asm__("{symbol}");\n'
                f'void stub_{i}(void) {{}}'
                for i, symbol in enumerate(symbols)
            ) + "\n")
            subprocess.run([
                compiler, "-shared", "-fPIC", "-nostdlib",
                "-Wl,-soname," + metadata["soname"],
                str(source), "-o", str(output / library),
            ], check=True)


if __name__ == "__main__":
    main()
