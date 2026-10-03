# Hoki network availability daemon

`hoki-networkd` publishes physical-network availability by starting one of two
mutually conflicting targets: `hoki-network-online.target` and
`hoki-network-offline.target`. It submits nonblocking systemd transactions and
never launches, monitors, signals, or reaps Tailscale/Connect processes. Target
definitions contain no consumer names. Systemd supervises the services and runs
post-stop cleanup before any replacement process.

Each consumer declares `Requisite=hoki-network-online.target`,
`After=hoki-network-online.target`, and `[Install] WantedBy=hoki-network-online.target`.
Enabling it installs a wants symlink. Starting the online target starts enabled
consumers; explicitly stopping online (including through the offline conflict)
stops them. Requisite prevents a consumer start from manufacturing availability.
Requires/BindsTo would pull online in, potentially displacing offline.
Both targets use DefaultDependencies=no plus explicit shutdown conflicts/ordering
to avoid default target-after-consumer ordering cycles.

Tailscale runs directly as a system Type=notify service; Connect runs directly in
the ceres user manager. Both retain Restart=on-failure and use TimeoutStopSec=60s.
The same small watcher runs once in each manager, using --user in ceres. Each
updates only its own manager, avoiding dependencies across system/user managers
or a requirement that the user bus be available when the system watcher starts.
Their online/offline state can briefly differ while delivering independent events.

A watcher initially offline immediately selects offline, without launching
consumers. An online mode survives short outages; two continuous minutes without
any qualifying IP select offline. Recovery cancels and resets the outage timer.
On recovery the watcher selects online immediately, including during consumer
termination/cleanup. Systemd queues the new start behind the existing teardown.
There is no Internet reachability test, default-route requirement, polling, or
radio control. The one-shot CLOCK_BOOTTIME timer counts suspend without waking
the watch solely for the deadline.

Eligible interfaces are up and running, backed by a sysfs device or wireless
interface, plus Hoki's USB NCM usb0. IPv4/IPv6 private and link-local addresses
qualify. Loopback, unspecified/multicast/broadcast addresses, tentative/DAD-failed
IPv6 addresses, overlays, tunnels and bridges do not qualify. Subscription to
rtnetlink precedes the initial interface snapshot.

Watcher termination/errors withdraw online directly. Its service ExecStopPost
also stops online, covering forced termination. This deliberately avoids starting
offline during system shutdown, because that target conflicts with shutdown.target.
Both targets may be inactive when the publisher is stopped: no availability is
then asserted. Restarting the watcher republishes a fresh snapshot.

`hoki-networkd --check` prints qualifying interface names, returning 0 if any,
1 otherwise, without changing services. The default loss grace is 120 seconds;
--loss-grace-seconds and --unit-prefix are explicit integration-test controls.

Validation from this project directory:

```
cargo test --locked --bin hoki-networkd
cargo build --locked --bin hoki-networkd
unshare -Urnm python3 tests/network-gate.py ../target/debug/hoki-networkd
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf'
```

The lifecycle test requires a real host user systemd manager. It creates uniquely
named runtime units, enables a mock consumer, and removes those units afterward.
Network manipulation is restricted to a private user/mount/network namespace.
The watcher uses 1-second loss grace, and the mock consumer uses 2-second stop
timeout. Tests cover offline consumer rejection, overlays, IPv4/IPv6, multiple
addresses, transient-outage reset, crash restart, recovery during graceful/forced
termination and ExecStopPost, rapid online/offline job reversal, and watcher exit.
Production defaults and loss boundary/reset behavior are checked separately.

Packaging installs the target pair and watcher in both managers. The user watcher
is enabled through default.target; the system watcher through multi-user.target.
Connect is enabled through the user online target. Tailscale personalization and
managed-rootfs boot enable it through the system online target when the new target
is present, retaining the old direct boot link for older images. Existing direct
boot enablement must be migrated before live deployment.

`tailscaled --cleanup` in pinned v1.102.4 invokes DNS cleanup and Linux router
cleanup before exiting, without starting the daemon or logging the node out.
DNS cleanup restores the prior system DNS configuration where supported. Linux
router cleanup removes Tailscale iptables/nftables state and stale Tailscale-range
addresses from its interface; failures may be logged best-effort. It does not
delete `/var/lib/tailscale/tailscaled.state`. Only run it after the old daemon
has exited, and finish it before launching a replacement.

Source: [daemon cleanup dispatch](https://github.com/tailscale/tailscale/blob/v1.102.4/cmd/tailscaled/tailscaled.go),
[DNS cleanup](https://github.com/tailscale/tailscale/blob/v1.102.4/net/dns/manager.go),
[Linux cleanup](https://github.com/tailscale/tailscale/blob/v1.102.4/wgengine/router/osrouter/router_linux.go).
