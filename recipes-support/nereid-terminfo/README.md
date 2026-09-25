# SSH terminal descriptions

BitBake compiles the checked-in text with `ncurses-native` tic; no host binary
terminfo database is copied into the image.

- `kitty.terminfo`: upstream kitty `terminfo/kitty.terminfo`, commit
  `c7d4eab814e473a9c2352609ebe6892a7a46da9d`, GPL-3.0-only.
  https://github.com/kovidgoyal/kitty/blob/c7d4eab814e473a9c2352609ebe6892a7a46da9d/terminfo/kitty.terminfo
- `ghostty.terminfo`: Ghostty 1.3.1's installed definition, exported as text with
  `infocmp -x -I xterm-ghostty` (initial host-path comment removed), MIT.
  Upstream maintains the definition as Zig data:
  https://github.com/ghostty-org/ghostty/blob/v1.3.1/src/terminfo/ghostty.zig

The matching upstream license texts are retained beside the descriptions.
