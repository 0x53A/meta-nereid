#!/bin/sh
set -eu
mode=${1:-display}
case "$mode" in display|mem) ;; *) exit 2;; esac
test -x /usr/lib/hoki-lp-watchface
test -x /tmp/hoki-suspend-check-0182
systemctl --user -M ceres@ is-active --quiet nereid-compositor
systemctl is-active --quiet hoki-hwc-proxy
# Clear only our stale readiness marker before the client starts.
rm -f /run/user/1000/hoki-lp-ready
systemctl --user -M ceres@ stop nereid-compositor
systemctl stop hoki-hwc-proxy
systemd-run --unit=hoki-lp-client-0182 --collect --uid=ceres --property=RuntimeMaxSec=80 --property=TimeoutStopSec=5 --setenv=XDG_RUNTIME_DIR=/run/user/1000 --setenv=EGL_PLATFORM=hwcomposer /usr/lib/hoki-lp-watchface face 70
n=0
until test -f /run/user/1000/hoki-lp-ready; do
    n=$((n + 1))
    if test "$n" -ge 20; then echo 'LP readiness timed out' >&2; exit 1; fi
    sleep 1
done
echo "READY: LP face accepted; test mode=$mode"
if test "$mode" = mem; then
    cat /sys/kernel/debug/wakeup_sources > /tmp/lp-wakeup-before-0182.txt
    /tmp/hoki-suspend-check-0182 mem 25
    cat /sys/kernel/debug/wakeup_sources > /tmp/lp-wakeup-after-0182.txt
else
    sleep 12
fi
echo 'TRIAL done; ExecStopPost restores UI'
