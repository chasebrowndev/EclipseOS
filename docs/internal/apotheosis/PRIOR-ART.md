# Apotheosis — prior-art log

Procedure: T-00 §3.4, ADR 0112. Implementers never open another terminal's source. They
ask the `apoth-prior-art` agent, which reads clones under
`~/.cache/apotheosis-prior-art/`, answers in prose (approach, invariants, edge cases —
no code), and appends an entry here. The research dossiers in `research/` were produced
under the same rule and are the first place to look.

Licences of the studied projects: kitty GPL-3.0 · foot MIT · wezterm MIT · alacritty
Apache-2.0 · ghostty MIT · rio MIT · contour Apache-2.0 · xterm MIT-X11 · VTE LGPL ·
Konsole GPL-2.0+ · Windows Terminal MIT · tmux ISC · zellij MIT · iTerm2 GPL-2.0+.
For the GPL and LGPL projects, protocol **specifications and documentation** are the
only inputs for `apoth-image` (kitty graphics) and `apoth-input` (kitty keyboard);
their source is not consulted for those crates.

## Log

| Date | Asked by (milestone, crate) | Question | Sources read (project, path) | Answer recorded in |
|---|---|---|---|---|
| 2026-11-12 | M0, research | 13 terminal dossiers and 9 topic studies | see each `research/*.md` "Sources" section | `research/` |
| 2026-11-14 | M32 (T-15 Graphics 2, tier A), terminal graphics | kitty graphics protocol animation (a=f/a/c, d=f), Unicode placeholders, relative placements, icat options; mpv --vo=kitty behaviour; iTerm2 OSC 1337 File= | kitty `docs/graphics-protocol.rst`, `gen/rowcolumn-diacritics.txt`, `kittens/icat/main.py` option text (specification/CLI docs only); mpv and iTerm2 from published docs/memory (no clone) | answered inline |
| 2026-11-14 | apoth-w17 worktree (milestone not stated), open-actions and hints | How kitty, WezTerm, foot and ghostty pick the opener for file paths and URLs: MIME detection, rule precedence, ssh/remote handling, relative path and path:line resolution, missing files, injection, reaping, activation tokens | kitty `kitty/open_actions.py`, `kitty/guess_mime_type.py`, `kitty/utils.py`, `kitty/window.py`, `kitty/boss.py`, `kittens/hints/main.py`; wezterm `wezterm-open-url/src/lib.rs`, `wezterm-gui/src/termwindow/mod.rs`, `config/src/config.rs`, `docs/hyperlinks.md`; foot `spawn.c`, `url-mode.c`, `config.c` (plus research/foot.md); ghostty `src/os/open.zig`, `src/Surface.zig`, `src/termio/stream_handler.zig`, `src/apprt/action.zig` | answered inline |
