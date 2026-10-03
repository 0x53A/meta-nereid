#!/usr/bin/env bash
# Host-only validation. No watch, QSEE, cryptsetup or mount calls.
# Author: Lukas Rieger <code@lukasrieger.com>
set -euo pipefail
cd -- "$(dirname -- "$0")"
mkdir -p build
nix-shell --arg nativeOnly true --run 'set -euo pipefail
cc -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror -fsanitize=address,undefined -g src/native/test-keymaster.c -o build/test-keymaster
build/test-keymaster
cc -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror -fsanitize=address,undefined -g src/native/test-ipc-protocol.c -o build/test-ipc-protocol
build/test-ipc-protocol
cc -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror -fsanitize=address,undefined -g src/native/test-gatekeeper-management.c -o build/test-gatekeeper-management
build/test-gatekeeper-management
python3 -m unittest discover -s src/native -p test_supervisor.py'
