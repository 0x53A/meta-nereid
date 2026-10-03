#!/usr/bin/env bash
# Run a build command with files owned by the invoking user.
set -euo pipefail
runtime=$1
build_dir=$2
shift 2
case "$runtime" in
    podman) identity=(--userns keep-id) ;;
    docker)
        # BitBake isolates tasks with unprivileged user/network namespaces.
        # Docker's default seccomp policy blocks the required unshare call.
        identity=(--user "$(id -u):$(id -g)" --env HOME=/tmp
            --security-opt seccomp=unconfined
            -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro)
        ;;
    *) echo 'Container runtime must be docker or podman' >&2; exit 1 ;;
esac
exec "$runtime" run --rm --interactive=false --tty=false \
    -v "$build_dir:/asteroid:z" "${identity[@]}" \
    -w /asteroid/asteroid asteroidos-toolchain "$@"
