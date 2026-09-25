#!/usr/bin/env bash
# Build the Linux x86_64 client and publish it at the documented launch path.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

nix-shell ../shell.nix --run '
    set -euo pipefail
    export RUSTUP_TOOLCHAIN=stable
    export CARGO_TARGET_DIR="$PWD/target"
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$PWD/../../../host-linker.sh"
    cargo build --locked --release --target x86_64-unknown-linux-gnu
'

mkdir -p target/release
staged_client=$(mktemp target/release/.ble-ssh-client.XXXXXX)
trap 'rm -f -- "$staged_client"' EXIT
install -m 755 target/x86_64-unknown-linux-gnu/release/ble-ssh-client "$staged_client"
mv -f -- "$staged_client" target/release/ble-ssh-client
