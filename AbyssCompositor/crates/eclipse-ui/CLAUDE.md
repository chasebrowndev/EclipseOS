# eclipse-ui — the design system

Read the root `CLAUDE.md` first.

- **Not TCB.** Nothing here is trusted; trusted UI is compositor-drawn
  (root invariants). A widget in this crate that would only make sense if it
  could be trusted belongs in `abyss/src/trusted_ui/` instead.
- `docs/STYLE.md` is the art direction and `tokens.rs` is its only
  transcription. A literal colour, radius or size anywhere else in the DE is a
  bug — add a token.
- `docs/COMPOSITION.md` is the other half: pane anatomy, the hero catalogue and
  the accent ledger. Correct tokens with no hero block is how a pane ends up
  looking generic. `docs/design/eclipse-panes.html` is the vendored reference
  it was derived from.
- Glass blur is the compositor's (`decoration { blur }`, dual-Kawase, on by
  default, applied to layer-shell surfaces as well as windows). This crate
  paints the translucent fill and border that sit *on* the blur; it never
  tries to blur anything itself.
- Corner radius follows the compositor: `ipc::fetch_config_radius` reads
  `decoration.rounding` (or `bar.rounding` for the taskbar) over `get_config`,
  and `theme::{panel, surface, menu_surface, bar_ground}` take that radius.
  This is why the crate depends on `eclipse-ipc`.
- Accent discipline: one live yellow per pane. Widgets take their accent from
  the theme, so a pane that looks wrong is a pane using two accent widgets,
  not a widget to restyle.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
