<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ec-desktop — the desktop's icons

Read the root `CLAUDE.md` first. Governing spec: C-24, COMP-17 §2, ADR 0090.
Its own crate and package (ADR 0052); never depend on `hyperion`.

- **Not TCB.** An ordinary layer-shell client on `Layer::Bottom` (above the wallpaper
  on Background, whatever maps first), one
  surface per output, following the wallpaper's per-output pattern
  (`app::reconcile`). It holds no capability and grants none.
- **The menu, the launch prompt and the failure note are not drawn on the
  Bottom surface** (windows would cover them): each is its own short-lived
  `Layer::Overlay` surface (`float.rs`, `app::sync_floats`). The menu/prompt
  sheet covers the same area as the output's surface, holds the keyboard
  (OnDemand) while open and closes on a press outside; the note is a small
  corner surface with no keyboard. An `xdg_popup` would not do: it stacks with
  its parent's layer.
- **Only under `mode "de"`.** `ec-desktop.service` has no `[Install]`;
  abyss starts and stops it (`ec-abyss/src/session.rs`, `desktop_sync`).
- **The model is non-visual and unit-tested**: `scan` (the directory and its
  watcher), `launch` (`.desktop` and `xdg-open`), `icons`, `ops` (file
  operations), `trash` (freedesktop trash spec), `layout` (grid positions per
  output). The view (`view.rs`) only draws it.
- **Nothing is ever overwritten and nothing is ever deleted for good.** A
  collision gets a ` (2)` suffix; delete is a move to the trash; moves
  across filesystems are refused rather than copy-then-delete.
- **A `.desktop` file launches only if it is executable** (the human's "allow
  launching", `ops::allow_launching`), so a download dropped on the desktop
  never runs on a double-click.
- **Never log a file name or path**, nor a launcher's contents: counts and
  error kinds only.
- No chrome beyond tokens: no literal colour.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
