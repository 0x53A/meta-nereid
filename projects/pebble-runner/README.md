# Pebble runtime

The maintained Pebble application/watchface runtime lives here. It was moved
from `_Tasks/0022_Pebble_Compat/pebble-runner`; the historical investigation and
headless test results remain in [task 0022](../../../_Tasks/0022_Pebble_Compat/readme.md).
The Pebble OS reference source is the root `pebble-os/` submodule.

Build from this directory using its Nix environment:

```sh
nix-shell --run 'cargo build --release --target armv7-unknown-linux-gnueabihf'
nix-shell -p patchelf --run 'bash ../../patch-watch-elf.sh ../target/armv7-unknown-linux-gnueabihf/release/pebble-runner'
```

For host tests, run `nix-shell --run 'cargo test --locked'`. Cargo fetches the
pinned armagnac fork revision, including the required ARM instruction extensions.
Required `.pfo` font assets are included
under `fonts/`; downloaded `.pbw` test applications remain optional local inputs.

For headless visual checks, set `PEBBLE_FRAME_CAPTURE_DIR` when using `--load`.
The runner saves completed frames at about 0.6, 2, 5, 10, and 30 seconds as PNGs in
that directory. Apps that stop before drawing a frame produce no PNG.
Chalk uses 180 × 180 guest coordinates. Basalt, Diorite, and Aplite use
144 × 168 coordinates and are fitted within the round host display.
For companion diagnostics, `PEBBLE_TEST_INBOX=7=1,1=0;2=123` injects two
AppMessage messages one second apart; semicolons group unsigned integer tuples
within one message. `PEBBLE_TEST_BUTTON=2` sends Select after one second, or
after `PEBBLE_TEST_BUTTON_DELAY_MS` when set.
Piny Wings' bundled KiezelPay companion is supported by a native adapter. It
uses a stable account token stored at `$XDG_DATA_HOME/pebble-runner/account-token`
(or `~/.local/share/pebble-runner/account-token`). Set `PEBBLE_ACCOUNT_TOKEN`
to use an existing token. The adapter forwards the service's actual status;
it does not unlock a purchase.

Image packaging uses [runtime-projects.txt](../../runtime-projects.txt).
Watch deployment and service ownership are documented in [CLAUDE.md](../../../CLAUDE.md).

## Watch UI

The 416 × 416 circular UI has a home screen, installed library, Rebble store
lists, preview, and live Pebble display. Library and store rows inset and scale
as they move toward the round bezel. Home actions and bottom navigation use
straight bands whose SVG edges follow the screen circle; the preview and other
controls have square corners. The SVG sources live under `ui/bands/`.
Each band uses the display circle centered at (208, 208) with radius 208; keep
the SVG viewBox coordinates aligned with the control's screen position when
changing the layout.
Store loading, empty, failed download, and installed states are visible on the
watch. A preview uses Music-sized split bottom controls: Back on the left,
and Install, Use, Launch, or download status on the right. **My library** marks
watchfaces and apps in each row; tapping either opens the compact live preview.
Use activates a watchface and returns to the list. Launch opens an app
fullscreen. Store previews offer the same actions after installation.
Fullscreen maps the top physical button to Pebble Up and the
bottom button to Down; crown rotation also sends Up/Down. **Emu settings** on
the home screen can enable a touch Select button. A touch Back button appears
automatically when the app registers a Back click handler. Pressing the crown
returns to the watch launcher; it does not deliver an overridden Back click.
The compact preview keeps the Pebble
framebuffer pixelated and shows touch Up, Select, and Down only for apps.
Pebble's SDK does not support button interaction in watchfaces. Browser lists scroll
by touch; the bottom Back control returns home.

A store download is first written to a `.download` file and is renamed to `.pbw`
only after a complete nonempty transfer. Temporary files are not shown as
installed apps.
Valid installed PBWs get user desktop shortcuts in
`$XDG_DATA_HOME/applications` (or `~/.local/share/applications`). The launcher
groups them under **Pebble apps**. Pebble Runner creates and removes its own
shortcuts at startup, when opening the installed library, and after a download;
it identifies its files with `X-ManagedBy=pebble-runner` and leaves other
desktop files alone. A shortcut opens the PBW in the normal
Pebble UI via `pebble-runner --app-id <encoded filename>`. Watchfaces are
included and can still be selected as the active face from the running view.
The Watch apps collection uses Rebble's `watchapps-and-companions` route; the
shorter `watchapps` path returns HTTP 404.
