# hoki-radiod

D-Bus system service daemon for controlling WiFi/BT radio mode on a Fossil Gen 6 (hoki) smartwatch running AsteroidOS.

Owns `org.hoki.radio` on the system bus, exposes `org.hoki.radio.Manager` at `/org/hoki/radio`.

## Methods

- `Status() -> (String, bool)`: `wifi`, `bt`, `wifi+bt`, or `off`; bool is WiFi Powered (persisted by ConnMan).
- `SetWifiEnabled(bool)`, `SetBluetoothEnabled(bool)`: independent radio controls.
- `SwitchToWifi()`, `SwitchToBt()`: compatibility aliases that enable the named radio, preserving the other.
- `DisableRadio()`, `EnableRadio()`: enter/leave ConnMan OfflineMode.
- `SetUsbMode(String)`: request a supported mode from usb-moded without directly modifying the gadget.
- `Reboot()`: reboot the watch.

Mutation methods return `ok` or an error string; Status returns a D-Bus error if ConnMan is unavailable. No manual supplicant, DHCP client, BT HAL teardown or WLAN module unloading. Hoki's patched WLAN driver permits coexistence; SW-PTA must remain enabled (task0054).

## Connection ownership

ConnMan owns association, credentials, autoconnect and failure handling. Radiod
only forwards explicit user actions; it does not clear connection errors or run
background retries. A service left in failure needs an explicit reconnect
through ConnMan. The separate ConnMan interface-readiness patch fixes driver
interface recreation; it does not change retry policy.

ConnMan calls allow 25 seconds: measured Hoki WLAN teardown can take 15.36
seconds, exceeding the old 15-second deadline (Settings allows 30 seconds).
Settings reads ConnMan OfflineMode for airplane mode; both radios being powered
off does not imply OfflineMode.

Tests: run `cargo test --locked`, then
`dbus-run-session -- cargo test --locked -- --ignored --test-threads=1`
for isolated ConnMan radio-control tests.

## Build

Cross-compile for the watch (armv7):

```sh
cargo build --release --target armv7-unknown-linux-gnueabihf
```

## Deploy

```sh
scp target/armv7-unknown-linux-gnueabihf/release/hoki-radiod root@hoki.local:/usr/local/bin/
scp deploy/org.hoki.radio.conf root@hoki.local:/etc/dbus-1/system.d/
scp deploy/org.hoki.radio.service root@hoki.local:/usr/share/dbus-1/system-services/
scp deploy/hoki-radiod.service root@hoki.local:/etc/systemd/system/
ssh root@hoki.local "systemctl daemon-reload && systemctl enable hoki-radiod.service"
```
