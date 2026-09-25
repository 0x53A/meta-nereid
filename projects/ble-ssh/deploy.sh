#!/bin/sh
# Convenience wrapper: cross-compile and deploy watch daemon
cd "$(dirname "$0")/watch-rs" && nix-shell --run ./deploy.sh
