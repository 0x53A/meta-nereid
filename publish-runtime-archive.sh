#!/usr/bin/env bash
# Pack and verify one staged payload without sharing a temporary archive name.
set -euo pipefail
if [ "$#" -ne 4 ]; then
    echo 'usage: publish-runtime-archive.sh STAGE PREFIX ARCHIVE PYTHON_CHECKER' >&2
    exit 2
fi
stage=$1
prefix=$2
archive=$3
checker=$4
temporary=$(mktemp "${archive}.tmp.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT
tar -C "$stage" -czf "$temporary" -- "$prefix"
python3 "$checker" "$temporary"
# Runtime archives contain distributable binaries/scripts, and existing builders
# publish them as 0644. Keep mktemp private until verification succeeds.
chmod 0644 "$temporary"
mv -fT -- "$temporary" "$archive"
