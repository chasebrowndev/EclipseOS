# eclipse-ui — the design system

Read the root `CLAUDE.md` first.

- **Not TCB.** Nothing here is trusted; trusted UI is compositor-drawn
  (root invariants). A widget in this crate that would only make sense if it
  could be trusted belongs in `abyss/src/trusted_ui/` instead.
- `/home/chase/Downloads/eclipse-style-spec.md` is the art direction and
  `tokens.rs` is its only transcription. A literal colour, radius or size
  anywhere else in the DE is a bug — add a token.
- Glass blur is the compositor's (`decoration { blur }`, dual-Kawase). This
  crate paints the translucent fill and border that sit *on* the blur; it
  never tries to blur anything itself.
- Accent discipline: one live yellow per pane. Widgets take their accent from
  the theme, so a pane that looks wrong is a pane using two accent widgets,
  not a widget to restyle.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
