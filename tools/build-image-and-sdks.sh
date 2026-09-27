#!/usr/bin/env bash
# Run inside the configured AsteroidOS build container.
set -eo pipefail

if [ "$#" -ne 1 ] || [[ ! "$1" =~ ^[1-9][0-9]*$ ]]; then
    echo "usage: build-image-and-sdks.sh BITBAKE_THREADS" >&2
    exit 2
fi

cd /asteroid/asteroid/build
. ../src/oe-core/oe-init-build-env . >/dev/null
set -u
export BB_NUMBER_THREADS="$1"

bitbake asteroid-image
bitbake -c populate_sdk asteroid-image
bitbake -c populate_sdk nereid-small-sdk

sdk_deploy=tmp/deploy/sdk
artifact_list="$sdk_deploy/nereid-sdk-artifacts.txt"
: > "$artifact_list"
for spec in nereid-full-sdk:asteroid-image nereid-small-sdk:nereid-small-sdk; do
    variant=${spec%%:*}
    recipe=${spec#*:}
    stem=$(bitbake -e "$recipe" | sed -n 's/^TOOLCHAIN_OUTPUTNAME="\([^"]*\)"$/\1/p' | tail -1)
    if [ -z "$stem" ] || [[ ! "$stem" =~ ^[A-Za-z0-9_.+-]+$ ]]; then
        echo "Invalid SDK output name for $recipe: $stem" >&2
        exit 1
    fi
    for suffix in sh host.manifest target.manifest testdata.json; do
        test -s "$sdk_deploy/$stem.$suffix"
    done
    (cd "$sdk_deploy" && sha256sum "$stem.sh" > "$stem.sh.sha256")
    printf '%s %s\n' "$variant" "$stem" >> "$artifact_list"
done
