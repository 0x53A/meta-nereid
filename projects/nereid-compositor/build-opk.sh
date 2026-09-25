#!/bin/sh
# Build nereid-compositor + hoki-launcher + hoki-hwc-proxy and package as .opk for AsteroidOS
# Usage: nix-shell --run ./build-opk.sh
set -e

TARGET=armv7-unknown-linux-gnueabihf
BINARY=target/$TARGET/release/nereid-compositor
LAUNCHER_DIR=../hoki-launcher
LAUNCHER_BINARY=$LAUNCHER_DIR/target/$TARGET/release/hoki-launcher
PROXY_DIR=../hoki-hwc-proxy
PROXY_BINARY=$PROXY_DIR/target/$TARGET/release/hoki-hwc-proxy
OPK_DIR=build-opk
OPK_NAME=nereid-compositor_0.1.0_armv7vehf-neon.opk

echo "==> Building compositor for $TARGET..."
cargo build --release --target "$TARGET"

echo "==> Building launcher for $TARGET..."
(cd "$LAUNCHER_DIR" && cargo build --release --target "$TARGET")

echo "==> Building HWC proxy for $TARGET..."
(cd "$PROXY_DIR" && cargo build --release --target "$TARGET")

echo "==> Patching binaries..."
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "$BINARY"
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "$LAUNCHER_BINARY"
patchelf --set-interpreter /lib/ld-linux-armhf.so.3 \
         --set-rpath /usr/lib:/lib \
         "$PROXY_BINARY"

echo "==> Assembling opk..."
rm -rf "$OPK_DIR"
mkdir -p "$OPK_DIR/data/usr/local/bin"
mkdir -p "$OPK_DIR/data/usr/lib"
mkdir -p "$OPK_DIR/data/usr/lib/systemd/user/default.target.wants"
mkdir -p "$OPK_DIR/data/etc/systemd/system/multi-user.target.wants"
mkdir -p "$OPK_DIR/data/var/lib/environment/compositor"
mkdir -p "$OPK_DIR/control"

# Compositor binary
cp "$BINARY" "$OPK_DIR/data/usr/local/bin/nereid-compositor"

# HWC proxy binary
cp "$PROXY_BINARY" "$OPK_DIR/data/usr/local/bin/hoki-hwc-proxy"

# Launcher binary (compositor spawns it from /usr/lib/hoki-launcher)
cp "$LAUNCHER_BINARY" "$OPK_DIR/data/usr/lib/hoki-launcher"

# HWC proxy system service + enable symlink (starts before compositor)
cp opk/hoki-hwc-proxy.service "$OPK_DIR/data/etc/systemd/system/"
ln -sf ../hoki-hwc-proxy.service "$OPK_DIR/data/etc/systemd/system/multi-user.target.wants/hoki-hwc-proxy.service"

# Compositor user service + enable symlink
cp opk/nereid-compositor.service "$OPK_DIR/data/usr/lib/systemd/user/"
ln -sf ../nereid-compositor.service "$OPK_DIR/data/usr/lib/systemd/user/default.target.wants/nereid-compositor.service"

# Compositor environment config
cp opk/default.conf "$OPK_DIR/data/var/lib/environment/compositor/default.conf"

# RSB enable service (system service — enables crown, scroll, and side buttons)
cp opk/hoki-rsb-enable.service "$OPK_DIR/data/etc/systemd/system/"
ln -sf ../hoki-rsb-enable.service "$OPK_DIR/data/etc/systemd/system/multi-user.target.wants/hoki-rsb-enable.service"

# Control file
cp opk/control "$OPK_DIR/control/control"

# postinst: stop old compositor + MCE, reload systemd, start new one
cat > "$OPK_DIR/control/postinst" << 'POSTINST'
#!/bin/sh
# Stop MCE — we handle display power directly
systemctl stop mce.service 2>/dev/null || true
systemctl mask mce.service 2>/dev/null || true

# Stop the old Qt compositor if running
systemctl --user stop asteroid-launcher.service 2>/dev/null || true
systemctl --user disable asteroid-launcher.service 2>/dev/null || true

# Disable mapplauncherd boosters
systemctl --user stop booster-generic.service booster-qt5.service booster-qtcomponents-qt5.service 2>/dev/null || true
systemctl --user disable booster-generic.service booster-qt5.service booster-qtcomponents-qt5.service 2>/dev/null || true

# Enable RSB (crown, scroll, side buttons)
systemctl daemon-reload
systemctl enable hoki-rsb-enable.service 2>/dev/null || true
systemctl start hoki-rsb-enable.service 2>/dev/null || true

# Start HWC proxy (system service — must start before compositor)
systemctl enable hoki-hwc-proxy.service 2>/dev/null || true
systemctl start hoki-hwc-proxy.service 2>/dev/null || true

# Reload and start compositor (user service)
systemctl --user daemon-reload
systemctl --user enable nereid-compositor.service
systemctl --user start nereid-compositor.service
POSTINST
chmod +x "$OPK_DIR/control/postinst"

# prerm: stop compositor and proxy before removal, unmask MCE
cat > "$OPK_DIR/control/prerm" << 'PRERM'
#!/bin/sh
systemctl --user stop nereid-compositor.service 2>/dev/null || true
systemctl --user disable nereid-compositor.service 2>/dev/null || true
systemctl stop hoki-hwc-proxy.service 2>/dev/null || true
systemctl disable hoki-hwc-proxy.service 2>/dev/null || true
systemctl stop hoki-rsb-enable.service 2>/dev/null || true
systemctl disable hoki-rsb-enable.service 2>/dev/null || true
systemctl unmask mce.service 2>/dev/null || true
PRERM
chmod +x "$OPK_DIR/control/prerm"

# Build the opk (tar-based, like ipk/opk format)
cd "$OPK_DIR"
tar czf ../data.tar.gz -C data .
tar czf ../control.tar.gz -C control .
echo "2.0" > ../debian-binary
cd ..
ar r "$OPK_NAME" debian-binary control.tar.gz data.tar.gz
rm -f debian-binary control.tar.gz data.tar.gz
rm -rf "$OPK_DIR"

ls -lh "$OPK_NAME"
echo "==> Done: $OPK_NAME"
echo "    Install on watch: scp $OPK_NAME root@hoki.local:/tmp/ && ssh root@hoki.local 'opkg install --force-conflicts /tmp/$OPK_NAME'"
echo ""
echo "    Fast deploy (compositor only, proxy stays alive):"
echo "      scp $BINARY root@hoki.local:/tmp/nereid-compositor"
echo "      ssh root@hoki.local 'cp /tmp/nereid-compositor /usr/local/bin/ && systemctl --user restart nereid-compositor'"
