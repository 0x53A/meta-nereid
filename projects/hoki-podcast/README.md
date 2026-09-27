# Podcast player

The app keeps downloaded episodes and the most recent playback position under
`$XDG_DATA_HOME/hoki-podcast`, or `$HOME/.local/share/hoki-podcast` when
`XDG_DATA_HOME` is unset or relative.

Episode cache filenames use a SHA-256 key of the exact enclosure URL. Feed
reordering does not change their identity. A changed enclosure URL, including
changed query parameters, produces a new cache entry. Downloads become visible
under their final filename only after the transfer succeeds.

Tapping an uncached episode adds it to a sequential download queue without
blocking the list. Each row shows queued, download progress, ready, or failed
state. The Downloads tab shows all jobs queued in the current app session;
completed jobs remain there until exit. Tapping a failed row retries it, and
tapping a ready row starts playback. Downloads do not auto-play on completion.
The queue is in memory; completed audio files remain cached across launches.

Saved progress includes this episode key and resumes only the matching episode.
Progress snapshots replace the state file atomically, so ordinary write failures
preserve the previous snapshot. These frequent writes do not force a disk flush;
the latest snapshot is not guaranteed to survive sudden power loss.
Duration metadata accepts seconds or colon-separated times; decoder duration
takes precedence when available. Relative seek buttons use the actual playback
position, including when total duration is unknown.
If loading fails, pressing Play reports that no audio is loaded and asks you to
select the episode again to retry; it does not leave the player marked as playing.
The numeric feed index is retained for compatibility, but new saved progress and
selection restoration use the key. Old `ep_N.mp3` cache files remain untouched;
they lack identity metadata and are not automatically reused. Old index-only
state can restore the list selection, but its position is not applied to an
unverified episode. Episodes will need downloading once into the new cache.

Native `cargo test --locked` in this project's `nix-shell` covers storage,
request ordering, playback ownership, saved state, and headless Rodio seeking.
The tests require neither an audio device nor a display. Follow the root
`CLAUDE.md` for ARM builds and watch deployment.

## Interface review

The app opens on a scrollable Podcasts list. Select a subscription to see its
episodes; the top back button returns to the subscription list. Both lists follow
the circular inset and center emphasis rules in the root
`knowledge/ui-design-rules.md`, and their last row can scroll to the center.
The bottom cap shows two destinations from Shows, Player, and Queue, omitting the
current destination. The episode list also has a hexagonal refresh icon that
updates the selected subscription. Twig and The Pale Audiobook Project are
included as subscriptions; the queue and cache are shared across them.
The player uses Connect-style curved playback bands and centered metadata.
The app has its own aubergine, bright berry, and soft rose palette across list,
player, queue, loading, and empty states. The playing indicator changes only
after the audio worker confirms playback.

Synthetic preview states render without feed or audio access. On a desktop with
a display, capture the actual Slint renderer as a PPM image:

```sh
nix-shell --run 'cargo run --locked -- --preview list-playing --capture /tmp/podcast-list.ppm'
```

Other states are `shows`, `many-shows` (100 synthetic subscriptions), `pale`,
`list`, `list-playing`, `list-queue`, `queue`, `player`, `long`, `empty`, and
`loading`. The preview
does not use network data; real feed, touch, audio, and bezel behavior still need
verification on the watch after deployment is authorized.

For a native desktop session with the live feed and audio, pass `--windowed`.
The window uses the watch's 416 × 416 layout. Set `XDG_DATA_HOME` if its
downloads and progress should stay separate from your normal host state:

```sh
XDG_DATA_HOME=/path/to/desktop-test-data nix-shell --run 'cargo run --locked -- --windowed'
```

On a non-ARM desktop, this mode does not contact the watch-only sleep
coordinator. ARM playback still requires the coordinator, including if started
with `--windowed`.

Playback holds a connection-owned hoki-powerd CPU inhibitor while allowing
ambient display. Play/resume requires a successful grant; pause, stop, end and
errors release it. The audio thread checks coordinator connectivity independently
of UI polling and stops output if that protection is lost. See
[the sleep contract](../hoki-powerd/SLEEP.md).
