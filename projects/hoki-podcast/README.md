# Podcast player

The app keeps downloaded episodes and the most recent playback position under
`$XDG_DATA_HOME/hoki-podcast`, or `$HOME/.local/share/hoki-podcast` when
`XDG_DATA_HOME` is unset or relative.

Episode cache filenames use a SHA-256 key of the exact enclosure URL. Feed
reordering does not change their identity. A changed enclosure URL, including
changed query parameters, produces a new cache entry. Downloads become visible
under their final filename only after the transfer succeeds.

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
