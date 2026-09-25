#!/bin/sh
# Session-owned coordinator. Child enforces recording identity/durability/RTC guards.
set -eu
[ "$#" = 5 ] || exit 64
controller=$1
socket=$2
capture=$3
hal=$4
ssc=$5
parent=${HOKI_POWER_SUPERVISOR:?missing power supervisor}
for unit in "$parent" "$hal" "$ssc"; do
 case "$unit" in ''|*[!a-zA-Z0-9_.@-]*|-*) exit 64;; esac
 case "$unit" in *.service) :;; *) exit 64;; esac
done
case "$parent" in hoki-recording-power-*.service) :;; *) exit 64;; esac
[ "$(id -u)" = 0 ]
[ "$(systemctl show "$parent" -p MainPID --value)" = "$$" ]
[ "$(systemctl show "$parent" -p ActiveState --value)" = active ]
bound=" $(systemctl show "$parent" -p BindsTo --value) "
for recorder in "$hal" "$ssc"; do
 case "$bound" in *" $recorder "*) :;; *) exit 64;; esac
done
trap 'exit 0' INT TERM
failures=0
retries=0
while systemctl is-active --quiet "$hal" && systemctl is-active --quiet "$ssc"; do
 battery=$(cat /sys/class/power_supply/battery/status)
 usb=$(cat /sys/class/android_usb/android0/state)
 if [ "$battery" != Discharging ] || [ "$usb" != DISCONNECTED ]; then
  printf 'POWER_DEFER battery=%s usb=%s retry_seconds=30\n' "$battery" "$usb"
  sleep 30
  continue
 fi
 id=$(cat /proc/sys/kernel/random/uuid)
 child=hoki-recording-suspend-$id.service
 set +e
 systemd-run --unit="$child" --wait --collect --property=Type=exec \
  --property=RuntimeMaxSec=30 --property=TimeoutStopSec=5 --property=KillMode=control-group \
  --property="BindsTo=$parent $hal $ssc" --property="After=$parent $hal $ssc" \
  --setenv="HOKI_SUSPEND_SUPERVISOR=$child" \
  "$controller" --suspend-recording-paced "$socket" "$capture"
 rc=$?
 set -e
 # Paced CLI: zero requires a returned call with >=0.5s estimated sleep.
 # Exit77 is a healthy deferral or too little measured sleep; keep its cooldown.
 if [ "$rc" = 0 ] || [ "$rc" = 77 ]; then
  failures=0
  retries=0
  if [ "$rc" = 77 ]; then
   printf 'POWER_COOLDOWN status=77\n'
   sleep 1
  fi
 elif [ "$rc" = 76 ]; then
  retries=$((retries+1))
  printf 'POWER_RETRY status=76 since_healthy_exit=%s\n' "$retries"
  # EBUSY may also be persistent driver trouble: never retry it indefinitely.
  [ "$retries" -lt 10 ] || exit 1
  sleep 1
 else
  failures=$((failures+1))
  printf 'POWER_ATTEMPT_FAILED status=%s consecutive=%s\n' "$rc" "$failures"
  [ "$failures" -lt 3 ] || exit 1
  sleep "$((failures*5))"
 fi
done
