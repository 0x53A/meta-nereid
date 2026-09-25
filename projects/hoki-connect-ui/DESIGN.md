# A reference for round watch apps

This app is deliberately a small set of well-proportioned screens. The watch
should show one task at a time, with a clear action and room to touch it.
`ui/main.slint` contains the reusable `Theme` and `BandButton` components.

## Geometry

The canvas is 416 × 416 logical pixels. The center is (208, 208); the active circle
has radius 208. Keep essential content inside radius 192. Check corners, not only
centers: the chord narrows quickly near the top and bottom. The dim outer rim is
decorative. It may approach the bezel; labels and touch centers must not.

- The section label sits at y=42, within a 216 px centered chord.
- The track title sits at y=180–244, centered at y=212 where the screen is widest,
  using 300 px between x=58 and 358. The source is above it at y=134–174,
  and artist below at y=246–270.
  Device titles use the same 352 px width, with name and status centered below
  a standalone 64 px laptop icon (no circular badge).
- Player switching uses 52×140 px bands at x=0 and 364, y=134–274. Their
  outer edges follow the circular screen; chevrons sit at y=208. Left selects
  the previous player, right the next; both wrap and appear only with multiple
  players. The full bands are touch targets, with no overlap into track controls.
  The center metadata also accepts horizontal swipes: left selects next, right
  previous. Require 60 px travel and horizontal distance greater than 1.5 times
  vertical distance; act once on release. Taps and cancelled gestures do nothing.
- Playback occupies y=72–132: three adjoining regions, 152/112/152 px
  wide before clipping, with 32 px icons and 60 px height. Play/pause owns the mint center segment.
  This top band is deliberately shorter than the volume band.
- Volume occupies y=276–344: decrease, 112 px centered readout, increase. The
  side regions have 68 px height and extend to the circular edge. The center is
  deliberately a readout, not an extra action.
- Device/Music divide the entire bottom circular cap at x=208, y=346–416.
  This begins 6 px higher than the original footer. Labels sit at y=371 and
  x=148/268, inside the narrowing chord; the surrounding visible area is tappable.
- Two-pixel horizontal gaps and fine vertical dividers separate adjoining bands.
  The SVG backgrounds use exact 208 px radius arcs, not straight trapezoid edges.
  Their source files are in `ui/bands/`; preserve the radius when resizing bands.
- The primary Device action is a single full-width band at y=276–344, aligned
  with Music’s volume strip. It shows Ping, Pair or Pairing according to actual
  state, retaining disabled gating. Navigation is shared across both pages,
  including disconnected and empty states.

For a scrolling list, use the existing project circular inset and center emphasis
rules in `knowledge/ui-design-rules.md`. They are intended for lists; scaling
fixed action labels by their distance from the center would obscure hierarchy.

## Typography and color

Use DejaVu Sans, available on the watch, with a restrained hierarchy:

| Role | Size / weight |
| --- | --- |
| Device title | 28 px / 600 |
| Track title | 25 px / 600, two lines |
| Primary action | 20 px / 600 |
| Status and explanation | 17 px / regular |
| Artist and source | 16 px / regular |
| Navigation | 19 px / 600 |
| Volume | 20 px / regular |
| Section eyebrow | 15 px / 600, 2 px letter spacing |

Black is the canvas, `#151d19` is the surface, `#edf3ef` is primary text,
`#a0aca5` is secondary text, and `#a8efce` is the only accent. Accent indicates
a primary action or current state. State also has words; color is not the sole
signal. Disabled actions dim and stop accepting input. Avoid oversized labels,
full-screen gradients, nested cards and icons made from punctuation.

SVG icons are local vector source, with consistent 24 px view boxes and rounded
strokes. Play/pause are optically centered filled shapes. No network artwork or
font-dependent icon glyphs. Text truncates within fixed bounds; long track titles
wrap to two lines and then elide. Do not shrink important text to fit arbitrary
remote metadata. Tap the source name to cycle players.

## Interaction and honest states

The GUI polls a private local socket on a worker thread. Network work and socket
waits never run on the UI thread. The daemon returns actual connection and media
state; pressing play does not optimistically claim playback changed. Pairing
requires a deliberate tap and laptop approval. A ping confirmation appears only
after the daemon sends the packet; it does not claim delivery acknowledgement.
Incoming pings appear briefly while this app is open, without waking the watch.

The crown adjusts volume on Music in 1% increments; touch controls retain 5%
steps. One raw crown tick forms one 1% increment, chosen after on-watch use. Ticks per physical revolution still need measurement. Page/player/connection changes
reset partial movement and pending targets. Unavailable controls discard movement.
Crown and touch volume update an immediate local target, separate from action busy.
A replaceable mailbox sends the latest absolute target at most every 100 ms;
there is no sequence of queued tick commands. Snapshots cannot overwrite the local
target for two seconds after the last input; afterward peer state wins.

Empty screens explain the next useful action. Disconnect hides media controls.
Old metadata may remain in daemon storage, but it is never presented as a live
player when disconnected. Only properties the peer supports enable actions.
Transient notices dismiss by tap or after three seconds. Press feedback changes the band background; there are no perpetual visual animations.

## Review before reusing

Inspect actual renderer captures for connected, disconnected, unpaired, pairing,
empty and long text states. Then inspect the watch compositor buffer and exercise
real touch targets. A square desktop preview does not establish physical bezel
clearance or readability on the wrist. Record those limits separately.

Current limits: one configured peer, no GUI address/fingerprint entry, no album
art, no system-volume control, and no automatic full-image/personalization
integration. GUI polling while open and daemon suspend/battery behavior still
need long-duration power validation.
