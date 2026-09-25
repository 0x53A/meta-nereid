#!/bin/sh
set -eu
# ExecStopPost is the independent recovery owner, including coordinator failure.
timeout 12 systemctl stop hoki-lp-client-0182.service || true
if systemctl is-active --quiet hoki-lp-client-0182.service; then
    echo 'LP client still owns graphics; refusing concurrent UI start' >&2
    exit 1
fi
timeout 20 /usr/lib/hoki-lp-watchface release 1
timeout 15 systemctl start hoki-hwc-proxy
timeout 15 systemctl --user -M ceres@ start nereid-compositor
echo 'RECOVERED: normal display services active; visibility requires observation'
