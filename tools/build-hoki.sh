#!/usr/bin/env bash
set -e

# Layer sources are independent of the caller; workspace contains sibling layers.
layer_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
repo_root=${NEREID_WORKSPACE:-$(dirname "$layer_root")}
cd "$repo_root"
REMOTE=${NEREID_BUILD_HOST:?Set NEREID_BUILD_HOST to an SSH build host}
REMOTE_DIR=${NEREID_BUILD_DIR:?Set NEREID_BUILD_DIR to a dedicated absolute build directory}
BB_THREADS=${NEREID_BUILD_THREADS:-6}
image_dir=${NEREID_IMAGE_DIR:-"$repo_root/images"}
# These values cross an SSH command boundary; accept unambiguous path/host syntax.
[[ "$REMOTE" =~ ^[A-Za-z0-9_][A-Za-z0-9_.@:-]*$ ]] || { echo 'Invalid build host' >&2; exit 1; }
[[ "$REMOTE_DIR" =~ ^/[A-Za-z0-9_./-]+$ && "$REMOTE_DIR" != / && "$REMOTE_DIR" != */.. && "$REMOTE_DIR" != */./* && "$REMOTE_DIR" != */./* && "$REMOTE_DIR" != */.. && "$REMOTE_DIR" != */./* && "$REMOTE_DIR" != */. ]] || { echo 'Use a dedicated absolute build directory without spaces or parent traversal' >&2; exit 1; }
[[ "$BB_THREADS" =~ ^[1-9][0-9]*$ ]] || { echo 'Build threads must be positive' >&2; exit 1; }
mkdir -p "$image_dir"
HOKI_CUSTOM_UI=${HOKI_CUSTOM_UI:-1}
HOKI_BLE_SSH=${HOKI_BLE_SSH:-1}
HOKI_ACOUSTIC_SSH=${HOKI_ACOUSTIC_SSH:-1}
for option in "$HOKI_CUSTOM_UI" "$HOKI_BLE_SSH" "$HOKI_ACOUSTIC_SSH"; do
    case "$option" in 0|1) ;; *) echo 'HOKI_CUSTOM_UI, HOKI_BLE_SSH and HOKI_ACOUSTIC_SSH must be 0 or 1' >&2; exit 1 ;; esac
done
# Acoustic SSH is fetched and compiled by its BitBake recipe.
if [ "$HOKI_BLE_SSH" = 1 ]; then
    archive="$layer_root/recipes-connectivity/ble-ssh/files/ble-ssh-runtime.tar.gz"
    if [ ! -s "$archive" ]; then
        echo 'Build Bluetooth SSH first: bash meta-nereid/build-ble-ssh.sh' >&2
        exit 1
    fi
    expected=$(python3 "$layer_root/ble-ssh-fingerprint.py")
    actual=$(tar -xOf "$archive" ble-ssh-runtime/usr/share/ble-ssh/source.sha256)
    if [ "$expected" != "$actual" ]; then
        echo 'Bluetooth SSH payload is stale; rerun meta-nereid/build-ble-ssh.sh.' >&2
        exit 1
    fi
fi
if [ "$HOKI_CUSTOM_UI" = 1 ] && [ ! -s "$layer_root/recipes-hoki/hoki-ui/files/hoki-runtime.tar.gz" ]; then
    echo 'Build the current custom UI first: bash meta-nereid/build-runtime.sh' >&2
    exit 1
fi

if [ "$HOKI_CUSTOM_UI" = 1 ]; then
    health_archive="$layer_root/recipes-hoki/hoki-health-recorder/files/health-recorder-runtime.tar.gz"
    if [ ! -s "$health_archive" ]; then
        echo 'Build health recorder tools first: bash meta-nereid/build-health-recorder.sh' >&2
        exit 1
    fi
    health_expected=$(python3 "$layer_root/health-recorder-fingerprint.py")
    health_actual=$(tar -xOf "$health_archive" health-recorder-runtime/usr/share/hoki-health-recorder/source.sha256)
    if [ "$health_expected" != "$health_actual" ]; then
        echo 'Health recorder payload is stale; rerun meta-nereid/build-health-recorder.sh.' >&2
        exit 1
    fi
    python3 "$layer_root/check-health-recorder.py" "$health_archive"
    expected=$(python3 "$layer_root/source-fingerprint.py")
    actual=$(tar -xOf "$layer_root/recipes-hoki/hoki-ui/files/hoki-runtime.tar.gz" hoki-runtime/usr/share/hoki/runtime-source.sha256)
    if [ "$expected" != "$actual" ]; then
        echo 'Custom UI payload is stale; rerun meta-nereid/build-runtime.sh.' >&2
        exit 1
    fi
    python3 "$layer_root/check-runtime.py" "$layer_root/recipes-hoki/hoki-ui/files/hoki-runtime.tar.gz"
fi

echo "=== Step 1: Rsync repositories to server ==="
# --delete keeps the staging copies exact mirrors (stale recipes otherwise
# linger and break bitbake parsing). asteroid/ is excluded from --delete's
# effect on src/ and build/ because those only exist server-side.
rsync -avz --delete --exclude '.git' --exclude 'src' --exclude 'build' asteroid/ "$REMOTE:$REMOTE_DIR/asteroid/"
rsync -avz --delete --exclude '.git' meta-asteroid/     "$REMOTE:$REMOTE_DIR/meta-asteroid/"
rsync -avz --delete --exclude '.git' meta-smartwatch/   "$REMOTE:$REMOTE_DIR/meta-smartwatch/"
rsync -avz --delete --exclude '.git' --exclude '/build/' --exclude '/projects/' --exclude '/tools/private/' --exclude '__pycache__' "$layer_root/" "$REMOTE:$REMOTE_DIR/meta-nereid/"
rsync -avz --delete --exclude '.git' --exclude '/build/' meta-hoki-ex/ "$REMOTE:$REMOTE_DIR/meta-hoki-ex/"
# These recipes build layer-owned sources directly. Other project sources are
# consumed by the local runtime builders; do not upload their caches or data links.
for project in nfcd-linux-plugin hoki-nfc-test-card hoki-gps-recorder; do
    ssh "$REMOTE" "mkdir -p '$REMOTE_DIR/meta-nereid/projects/$project'"
    rsync -avz --delete --exclude '.git' --exclude 'build' --exclude '__pycache__' \
        "$layer_root/projects/$project/" "$REMOTE:$REMOTE_DIR/meta-nereid/projects/$project/"
done

echo ""
echo "=== Step 2: Build container image ==="
ssh "$REMOTE" "podman build --tag asteroidos-toolchain $REMOTE_DIR/asteroid/"

echo ""
echo "=== Step 3: Setup build environment (if needed) ==="
# Remote login shells may not be bash — always go through `bash -s`.
ssh "$REMOTE" bash -s -- "$REMOTE_DIR" "$HOKI_CUSTOM_UI" "$HOKI_BLE_SSH" "$HOKI_ACOUSTIC_SSH" <<'REMOTE_SCRIPT'
    set -e
    REMOTE_DIR="$1"
    HOKI_CUSTOM_UI="$2"
    HOKI_BLE_SSH="$3"
    HOKI_ACOUSTIC_SSH="$4"

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
    sed -i '\|^BBLAYERS += "/asteroid/meta-hoki-local"$|d; \|^BBLAYERS += "/asteroid/meta-hoki-ex"$|d; \|^BBLAYERS += "/asteroid/meta-nereid"$|d' "$REMOTE_DIR/asteroid/build/conf/bblayers.conf"
    if [ "$HOKI_CUSTOM_UI" = 1 ] || [ "$HOKI_BLE_SSH" = 1 ] || [ "$HOKI_ACOUSTIC_SSH" = 1 ]; then
        printf '\nBBLAYERS += "/asteroid/meta-hoki-ex"\nBBLAYERS += "/asteroid/meta-nereid"\n' >> "$REMOTE_DIR/asteroid/build/conf/bblayers.conf"
    fi

    # Explicit selections prevent stale settings when switching image variants.
    sed -i '/^HOKI_CUSTOM_UI[[:space:]]*=/d; /^HOKI_BLE_SSH[[:space:]]*=/d; /^HOKI_ACOUSTIC_SSH[[:space:]]*=/d' "$REMOTE_DIR/asteroid/build/conf/local.conf"
    printf '\nHOKI_CUSTOM_UI = "%s"\nHOKI_BLE_SSH = "%s"\nHOKI_ACOUSTIC_SSH = "%s"\n' "$HOKI_CUSTOM_UI" "$HOKI_BLE_SSH" "$HOKI_ACOUSTIC_SSH" >> "$REMOTE_DIR/asteroid/build/conf/local.conf"

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
        bash -c "cd build && . ../src/oe-core/oe-init-build-env . >/dev/null && BB_NUMBER_THREADS=$BB_THREADS bitbake asteroid-image"
REMOTE_SCRIPT

echo ""
echo "=== Step 5: Rsync back images ==="
rsync -avz "$REMOTE:$REMOTE_DIR/asteroid/build/tmp/deploy/images/hoki/" "$image_dir/"

echo ""
python3 "$layer_root/tools/check-boot-image.py" \
    "$image_dir/asteroid-hoki-boot.img" \
    "$image_dir/initramfs-android-image-hoki.cpio.gz"

echo "=== Done! ==="
ls -la "$image_dir/"
