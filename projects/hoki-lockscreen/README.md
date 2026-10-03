# Hoki lock screen

Standalone Slint front end for the compositor's managed `lock-screen` role and
the PIN Management app launched by Settings.
`hoki-lockscreen` is one renderer implementation, not the role itself. A different
application can provide another visual style through the same role and shared
`io.Nereid.Auth1` service; it does not need its own authentication daemon.
The display is 416 × 416 with 84 × 56 touch keys, masked entry, PIN confirmation
for first enrollment and PIN change, a separate screen-lock disable confirmation,
pending/failure/retry feedback and a six-digit suggested PIN length. The shared
authentication service accepts 4–12 ASCII digits.

The application owns no credential database and performs no authentication.
Its worker calls `nereid-auth` for state, attempt setup and encrypted PIN
submission. Verification uses `SubmitPin`; PIN Management uses the enrollment
call for a new PIN and the authenticated management calls for PIN change or
clear. It emits no stdout unlock command. The compositor must observe the
shared service's authenticated state before releasing the managed role.

The managed role is verification-only. A healthy service reply with no enrolled
PIN and an unlocked state closes the renderer immediately; service errors leave
it locked. Running the binary as an ordinary app without arguments offers PIN
verification only and closes after successful verification. Settings launches
`hoki-lockscreen --manage-pin` as an ordinary app. Management requires a healthy,
unlocked service state. With no enrolled PIN it offers the existing two-entry
Create PIN flow. With an enrolled PIN it offers Change PIN, Clear PIN and Back.
Change collects the current PIN and a confirmed new PIN, then submits them to
the service for verification and change. Clear collects the current PIN, asks
for a separate confirmation that screen locking will be disabled, then submits
the request to the service; it leaves encrypted data untouched. If encrypted
storage is present the service refuses clear and the UI reports that result. Successful
change and clear keep the current session unlocked and show a result screen with
a Back to Settings action. The legacy `--enroll` argument remains an alias for
PIN Management.

The compositor starts one regular fullscreen xdg toplevel and matches it to the
managed child PID. It sends `visibility:visible` and `visibility:hidden` on
stdin; EOF ends the managed application. The role's stdout is ignored. After installing
the binary at `/usr/lib/hoki-lockscreen`, configure the ceres compositor using
its private control socket:

```text
set-lock-screen /usr/lib/hoki-lockscreen
```

Build from this project directory with its Nix shell:

```sh
nix-shell --arg nativeOnly true --run 'cargo test -p hoki-lockscreen --locked'
nix-shell --run 'cargo build -p hoki-lockscreen --locked --release --target armv7-unknown-linux-gnueabihf'
```

The crate uses its sibling `nereid-auth` library and the shared Slint 1.15.1
configuration. `--preview verify --capture /tmp/lock.ppm` renders a static
dummy state without connecting to any authentication service. Other preview
states are `enroll`, `confirm`, `manage`, `change-current`, `change-new`,
`confirm-new`, `clear-current`, `clear-confirm`, `clear-protected`, `changed`,
`cleared`, `pending`, `failure`, `retry`, and `unavailable`.
Preview is not an authentication mode and cannot unlock the compositor.

PIN bytes stay in a dedicated, page-aligned `mmap` allocation. The process
disables dumpability and requires `mlock` for that page before accepting digits.
The allocation holds the active entry, a new PIN or confirmation, and the saved
current PIN in separate slots. The worker receives the locked page by ownership,
wipes it before releasing it, and does not print request data. This protects only the buffers this
application owns: the Rust runtime, crypto implementation, IPC library and
kernel may make transient copies, and this does not protect against root, a
compromised compositor, forced termination, or a memory disclosure.

PIN Management keeps a separate Back control on entry screens; the management
menu and result screens each provide one clear Back action.
