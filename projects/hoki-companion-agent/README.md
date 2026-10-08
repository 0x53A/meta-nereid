# Hoki companion helper

One JSON line on stdin, one JSON response on stdout. Installed as
`/usr/libexec/hoki-companion-agent` by the `hoki-companion-agent` recipe, included
in the custom Hoki image. It is invoked by our Android app over host-key-verified
SSH and is also suitable for future laptop clients. No TCP listener or daemon.

Protocol v1 requests:

```json
{"version":1,"op":"status"}
{"version":1,"op":"sync_time","utc_ms":1800000000000,"timezone":"Europe/Berlin"}
{"version":1,"op":"save_wifi","ssid":"Example","security":"wpa-psk","password":"example-password","hidden":false}
```

Response is `{"ok":true,"result":{...}}` or `{"ok":false,"error":"..."}`.
The first line is limited to 8192 bytes. Unknown versions/operations/fields are
rejected. Error responses never echo a request, password or subprocess output.
Each subprocess has a timeout and runs without a shell. The Android SSH command
also bounds time and output. Authentication/authorization comes from SSH;
mutations require root, consistent with the current app's administrator setup.
This is not a restricted SSH account: the pre-existing app key grants root SSH.

`status` returns nullable battery/charging, ConnMan networks, timezone and a
subset of Tailscale status. `null` means unavailable, not zero/offline. It does
not test DNS, internet, or SSH from a different peer, nor start services.

`save_wifi` validates SSID length in UTF-8 bytes and WPA keys. Names are hex-encoded
and passphrases use GLib key-file escaping. Open/WPA personal only; no enterprise,
WEP or claimed WPA3 support. Files are named `hokicompanion<sha256>.config` under
`/var/lib/connman`, mode 0600, written via fsync + atomic replacement + directory
fsync. Root owns this directory. The temporary filename is not a ConnMan config.
Other provisioners' matching SSID entries are rejected and left untouched. A saved
response does not imply connection. Updating an existing provisioning file can
cause ConnMan to reprovision it, potentially interrupting its current connection;
a lost response is an uncertain result, never grounds for automatic retry.

`sync_time` validates an installed TZif timezone and dates in 2020–2099. It sets
timezone through timedated, then seeds CLOCK_REALTIME only if NTPSynchronized is
false and the difference exceeds two seconds. It accounts for local helper
processing delay but not transport latency. NTP configuration and monotonic clocks
are preserved. If timezone succeeds but setting time fails, the response reports
failure; the timezone change may have taken effect. Unsupported phone timezone
aliases are rejected rather than guessed.

Tests (from this project):

```sh
python3 -m unittest discover -s tests -v
```

The custom image recipe explicitly installs the Python modules used here. Manual
watch deployment/service changes require the project deployment authorization;
source implementation does not enable them.
