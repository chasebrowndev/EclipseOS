# eclipse-wallpaper — the desktop background

Read the root `CLAUDE.md` first. Its own crate and package (ADR 0052); never
depend on `hyperion`.

- **Not TCB.** An ordinary layer-shell client on `Layer::Background`, one
  surface per output, following hyperion's per-output daemon pattern
  (`app::reconcile`). It holds no capability and grants none.
- **It never exits over a picture.** A missing, unreadable or corrupt file is
  one warning on stderr and the colour alone.
- **Config comes from `get_config`**, read in exactly one place:
  `config::from_reply`. Every key is optional; anything absent or malformed
  keeps its default (`eclipse_ui::tokens::color::BASE`, `fill`, no image).
- No chrome: no text, no effects, no literal colour — the only colours are
  `tokens::color::BASE` and what the human configured.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
