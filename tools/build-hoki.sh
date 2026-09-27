#!/usr/bin/env bash
set -e

# Layer sources are independent of the caller; workspace contains sibling layers.
layer_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
repo_root=${NEREID_WORKSPACE:-$(dirname "$layer_root")}
cd "$repo_root"
REMOTE=${NEREID_BUILD_HOST:?Set NEREID_BUILD_HOST to an SSH build host}
REMOTE_DIR=${NEREID_BUILD_DIR:?Set NEREID_BUILD_DIR to a dedicated absolute build directory}
BB_THREADS=${NEREID_BUILD_THREADS:-6}
MAKE_JOBS=${NEREID_MAKE_JOBS:-4}
image_dir=${NEREID_IMAGE_DIR:-"$repo_root/images"}
# These values cross an SSH command boundary; accept unambiguous path/host syntax.
[[ "$REMOTE" =~ ^[A-Za-z0-9_][A-Za-z0-9_.@:-]*$ ]] || { echo 'Invalid build host' >&2; exit 1; }
[[ "$REMOTE_DIR" =~ ^/[A-Za-z0-9_./-]+$ && "$REMOTE_DIR" != / && "$REMOTE_DIR" != */.. && "$REMOTE_DIR" != */./* && "$REMOTE_DIR" != */./* && "$REMOTE_DIR" != */.. && "$REMOTE_DIR" != */./* && "$REMOTE_DIR" != */. ]] || { echo 'Use a dedicated absolute build directory without spaces or parent traversal' >&2; exit 1; }
[[ "$BB_THREADS" =~ ^[1-9][0-9]*$ ]] || { echo 'Build threads must be positive' >&2; exit 1; }
[[ "$MAKE_JOBS" =~ ^[1-9][0-9]*$ ]] || { echo 'Make jobs must be positive' >&2; exit 1; }
mkdir -p "$image_dir"
HOKI_CUSTOM_UI=${HOKI_CUSTOM_UI:-1}
HOKI_BLE_SSH=${HOKI_BLE_SSH:-1}
HOKI_ACOUSTIC_SSH=${HOKI_ACOUSTIC_SSH:-1}
for option in "$HOKI_CUSTOM_UI" "$HOKI_BLE_SSH" "$HOKI_ACOUSTIC_SSH"; do
    case "$option" in 0|1) ;; *) echo 'HOKI_CUSTOM_UI, HOKI_BLE_SSH and HOKI_ACOUSTIC_SSH must be 0 or 1' >&2; exit 1 ;; esac
done
# All runtime components are compiled by BitBake. Reject stale generated crate
# metadata before syncing rather than silently resolving newer dependencies.
python3 "$layer_root/tools/update-runtime-recipes.py" --check
if [ "$HOKI_CUSTOM_UI" = 1 ]; then
    python3 "$layer_root/check-runtime.py"
fi

echo "=== Step 1: Rsync repositories to server ==="
# --delete keeps the staging copies exact mirrors (stale recipes otherwise
# linger and break bitbake parsing). asteroid/ is excluded from --delete's
# effect on src/ and build/ because those only exist server-side.
rsync -avz --delete --exclude '.git' --exclude 'src' --exclude 'build' asteroid/ "$REMOTE:$REMOTE_DIR/asteroid/"
rsync -avz --delete --exclude '.git' meta-asteroid/     "$REMOTE:$REMOTE_DIR/meta-asteroid/"
rsync -avz --delete --exclude '.git' meta-smartwatch/   "$REMOTE:$REMOTE_DIR/meta-smartwatch/"
rsync -avz --delete --exclude '.git' meta-nereid-sdk/    "$REMOTE:$REMOTE_DIR/meta-nereid-sdk/"
rsync -avz --delete --exclude '.git' --exclude '/build/' --exclude '/projects/' --exclude '/tools/private/' --exclude '__pycache__' --exclude '*-runtime.tar.gz' "$layer_root/" "$REMOTE:$REMOTE_DIR/meta-nereid/"
rsync -avz --delete --exclude '.git' --exclude '/build/' meta-hoki-ex/ "$REMOTE:$REMOTE_DIR/meta-hoki-ex/"
# Stage only recipe inputs: projects also contain private data symlinks and
# development caches that must never be sent to the builder.
source_stage=$(mktemp -d)
trap 'rm -rf "$source_stage"' EXIT
python3 "$layer_root/tools/stage-project-sources.py" "$source_stage"
rsync -avz --delete "$source_stage/" "$REMOTE:$REMOTE_DIR/meta-nereid/projects/"

echo ""
echo "=== Step 2: Build container image ==="
ssh "$REMOTE" "podman build --tag asteroidos-toolchain $REMOTE_DIR/asteroid/"

echo ""
echo "=== Step 3: Setup build environment (if needed) ==="
# Remote login shells may not be bash — always go through `bash -s`.
ssh "$REMOTE" bash -s -- "$REMOTE_DIR" "$HOKI_CUSTOM_UI" "$HOKI_BLE_SSH" "$HOKI_ACOUSTIC_SSH" "$BB_THREADS" "$MAKE_JOBS" <<'REMOTE_SCRIPT'
    set -e
    REMOTE_DIR="$1"
    HOKI_CUSTOM_UI="$2"
    HOKI_BLE_SSH="$3"
    HOKI_ACOUSTIC_SSH="$4"
    BB_THREADS="$5"
    MAKE_JOBS="$6"

    if [ ! -d $REMOTE_DIR/asteroid/src/oe-core ] || [ ! -f $REMOTE_DIR/asteroid/build/conf/local.conf ]; then
        podman run --rm --interactive=false --tty=false \
            -v "$REMOTE_DIR:/asteroid:z" \
            --userns keep-id \
            -w /asteroid/asteroid \
            asteroidos-toolchain \
            bash -c '. ./prepare-build.sh hoki'
    fi

    test -d "$REMOTE_DIR/asteroid/src/oe-core"
    test -f "$REMOTE_DIR/asteroid/build/conf/local.conf"
    test -f "$REMOTE_DIR/asteroid/build/conf/bblayers.conf"

    # Always sync our local meta layers into the build tree
    rm -rf $REMOTE_DIR/asteroid/src/meta-smartwatch
    cp -r $REMOTE_DIR/meta-smartwatch $REMOTE_DIR/asteroid/src/meta-smartwatch

    rm -rf $REMOTE_DIR/asteroid/src/meta-asteroid
    cp -r $REMOTE_DIR/meta-asteroid $REMOTE_DIR/asteroid/src/meta-asteroid

    # Explicit local image policy; hardware layers stay usable upstream.
    sed -i '\|^BBLAYERS += "/asteroid/meta-hoki-local"$|d; \|^BBLAYERS += "/asteroid/meta-hoki-ex"$|d; \|^BBLAYERS += "/asteroid/meta-nereid"$|d; \|^BBLAYERS += "/asteroid/meta-hoki-sdk"$|d; \|^BBLAYERS += "/asteroid/meta-nereid-sdk"$|d' "$REMOTE_DIR/asteroid/build/conf/bblayers.conf"
    printf '\nBBLAYERS += "/asteroid/meta-nereid-sdk"\n' >> "$REMOTE_DIR/asteroid/build/conf/bblayers.conf"
    if [ "$HOKI_CUSTOM_UI" = 1 ] || [ "$HOKI_BLE_SSH" = 1 ] || [ "$HOKI_ACOUSTIC_SSH" = 1 ]; then
        printf '\nBBLAYERS += "/asteroid/meta-hoki-ex"\nBBLAYERS += "/asteroid/meta-nereid"\n' >> "$REMOTE_DIR/asteroid/build/conf/bblayers.conf"
    fi

    # Explicit selections prevent stale settings when switching image variants.
    sed -i '/^HOKI_CUSTOM_UI[[:space:]]*=/d; /^HOKI_BLE_SSH[[:space:]]*=/d; /^HOKI_ACOUSTIC_SSH[[:space:]]*=/d' "$REMOTE_DIR/asteroid/build/conf/local.conf"
    printf '\nHOKI_CUSTOM_UI = "%s"\nHOKI_BLE_SSH = "%s"\nHOKI_ACOUSTIC_SSH = "%s"\n' "$HOKI_CUSTOM_UI" "$HOKI_BLE_SSH" "$HOKI_ACOUSTIC_SSH" >> "$REMOTE_DIR/asteroid/build/conf/local.conf"

    # Cap both concurrent BitBake tasks and compile jobs inside each task.
    sed -i '/^BB_NUMBER_THREADS[[:space:]]*=/d; /^PARALLEL_MAKE[[:space:]]*=/d' "$REMOTE_DIR/asteroid/build/conf/local.conf"
    printf 'BB_NUMBER_THREADS = "%s"\nPARALLEL_MAKE = "-j%s"\n' "$BB_THREADS" "$MAKE_JOBS" >> "$REMOTE_DIR/asteroid/build/conf/local.conf"

    # Ensure MACHINE is set (local.conf is auto-generated by prepare-build.sh)
    grep -q 'MACHINE' $REMOTE_DIR/asteroid/build/conf/local.conf 2>/dev/null || \
        echo 'MACHINE = "hoki"' >> $REMOTE_DIR/asteroid/build/conf/local.conf

    # Remove stale local kernel config if present (kernel now fetched from git)
    sed -i '/USE_LOCAL_KERNEL/d; /LOCAL_KERNEL_DIR/d' $REMOTE_DIR/asteroid/build/conf/local.conf 2>/dev/null || true
REMOTE_SCRIPT

echo ""
echo "=== Step 4: Build ==="
ssh "$REMOTE" bash -s -- "$REMOTE_DIR" "$BB_THREADS" <<'REMOTE_SCRIPT'
    set -e
    REMOTE_DIR="$1"
    BB_THREADS="$2"

    podman run --rm --interactive=false --tty=false \
        -v "$REMOTE_DIR:/asteroid:z" \
        --userns keep-id \
        -w /asteroid/asteroid \
        asteroidos-toolchain \
        bash /asteroid/meta-nereid/tools/build-image-and-sdks.sh "$BB_THREADS"
REMOTE_SCRIPT

echo ""
echo "=== Step 5: Rsync back images ==="
rsync -avz "$REMOTE:$REMOTE_DIR/asteroid/build/tmp/deploy/images/hoki/" "$image_dir/"

# The Cargo precursors live in the image deploy tree, alongside the Yocto SPDX
# report. Keep them on the workstation for per-binary license enrichment.
# A stock build omits meta-nereid and has no Cargo report producers.
if [ "$HOKI_CUSTOM_UI" = 1 ] || [ "$HOKI_BLE_SSH" = 1 ] || [ "$HOKI_ACOUSTIC_SSH" = 1 ]; then
    test -d "$image_dir/cargo-sbom" || {
        echo 'Missing Cargo SBOM reports for the selected Nereid image' >&2
        exit 1
    }
fi

echo ""
echo "=== Step 6: Rsync back SDKs ==="
# Keep each SDK pair beside the rootfs version it was built with. SDK installers
# are compressed already, so rsync's compression brings little benefit.
rootfs_path=$(readlink -f "$image_dir/asteroid-image-hoki.rootfs.ext4")
test -f "$rootfs_path"
sdk_dir=${NEREID_SDK_DIR:-"$image_dir/sdk"}/$(basename "$rootfs_path" .ext4)
mkdir -p "$sdk_dir"
rsync -av "$REMOTE:$REMOTE_DIR/asteroid/build/tmp/deploy/sdk/nereid-sdk-artifacts.txt" "$sdk_dir/"
sdk_count=0
while read -r recipe stem; do
    case "$recipe" in nereid-full-sdk|nereid-small-sdk) ;; *) echo "Unexpected SDK variant: $recipe" >&2; exit 1 ;; esac
    [[ "$stem" =~ ^[A-Za-z0-9_.+-]+$ ]] || { echo "Invalid SDK artifact name: $stem" >&2; exit 1; }
    for suffix in sh sh.sha256 host.manifest target.manifest testdata.json; do
        rsync -av "$REMOTE:$REMOTE_DIR/asteroid/build/tmp/deploy/sdk/$stem.$suffix" "$sdk_dir/"
    done
    (cd "$sdk_dir" && sha256sum -c "$stem.sh.sha256")
    sdk_count=$((sdk_count + 1))
done < "$sdk_dir/nereid-sdk-artifacts.txt"
test "$sdk_count" -eq 2

echo ""
python3 "$layer_root/tools/check-boot-image.py" \
    "$image_dir/asteroid-hoki-boot.img" \
    "$image_dir/initramfs-android-image-hoki.cpio.gz"

echo "=== Done! ==="
ls -la "$image_dir/"
ls -lh "$sdk_dir/"
