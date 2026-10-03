#!/usr/bin/env bash
# Author: Lukas Rieger <code@lukasrieger.com>
set -euo pipefail
cd -- "$(dirname -- "$0")"
mkdir -p build
nix-shell --run 'set -euo pipefail
armv7l-unknown-linux-gnueabihf-gcc -I src/native/uapi -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror -O2 src/native/rpmb-listener.c -o build/rpmb-listener
armv7l-unknown-linux-gnueabihf-gcc -I src/native/uapi -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror -O2 src/native/gatekeeper-backend.c -o build/nereid-gatekeeper-backend
bash ../../patch-watch-elf.sh build/rpmb-listener
bash ../../patch-watch-elf.sh build/nereid-gatekeeper-backend'
