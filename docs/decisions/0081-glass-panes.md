# 0081 — Compositor-drawn glass panes for DE cells
Status: proposed
Date: 2026-10-31
Deciders: chase (owner), Claude (advisory)

## Context
Taskbar cells are window chips, pager tiles, widgets and the launcher. Each is
painted by the client through `theme::glass_cell`: a uniform white fill, an
even hairline and a drop shadow. Measured on screen, a cell is one flat grey
with no depth. The focused chip is a brighter flat slab (35 against 24 for a
plain cell), so it reads as an opaque button. Windows in glass mode get the
compositor's glass instead (COMP-02 §9, C-14/C-15): a backdrop bent across a
bevel, vibrancy and smoke, a hairline lit from the top-left with a weaker
bottom-right lobe, and a dark seam. The desktop draws two materials, and only
the compositor's reads as glass.

A client cannot draw that material, because it has no access to the backdrop.
`ext_background_effect_v1` (COMP-06 §1, BLUR-03) lets a client choose *where*
blur goes. It carries no radius, tone or bezel, its shapes are capped at four
unioned boxes, and it draws one lens, not one per cell.

## Options
1. **Imitate the bezel client-side.** Draw a directional hairline and a seam in
   iced. This needs no protocol. The glass still has no backdrop, so the cells
   stay flat with a nicer rim.
2. **Extend `ext_background_effect_v1`.** That protocol is upstream and only
   carries regions. Per-box radius and tone cannot be added to it.
3. **A new protocol, `eclipse_glass_v1`.** The client declares panes (rect,
   radius, bezel width, tone, state, alpha). The compositor draws its glass
   program once per pane over the surface's cached backdrop.

## Decision
We take **3**.

- **Contract.** `abyss/crates/ec-protocols/protocols/eclipse-glass-v1.xml`
  (Apache-2.0).
  - The pane list is double-buffered and latched on `wl_surface.commit`, so
    glass never lags the content it sits under.
  - At most 64 panes per surface.
- **Bind is DE clients only.** The manager global is filtered by `can_view`
  against a client-name allowlist, the screencopy pattern. Third-party apps keep
  the generic mechanisms of D-05 §9. This keeps ambient authority out, and keeps
  compositor GPU work from being requested by untrusted clients.
- **Capability.** Advertised only while `decoration.blur.mode` is `glass`.
  Without it, clients draw today's ground unchanged. That covers blur off,
  frost, and any other host.
- **Render.** The backdrop is blurred once per surface, over the union of its
  panes. The cache is keyed on that union, so animating a cell's width does not
  re-blur. The glass program then draws once per pane with the window-bezel
  uniforms: bend confined to the bezel band, dispersion off. Tone and state move
  only the rim and a small fill lift. No gradient anywhere, and gold never goes
  into the glass; the focused label keeps it.
- **Panes replace the surface's whole-layer glass.** A surface with no glass
  object renders exactly as before.

## Consequences
- **One material.** Windows and DE cells now share it, so a chip and a window
  bezel read as the same object.
- **Cost.** The backdrop blur is already paid for the bar. Each pane adds a
  small fragment pass over its own pixels, with a few taps on the bezel band
  and one on the plateau. That is well inside COMP-02 §10's blur budget, to be
  confirmed with `abyss/bench`.
- **Two paths to keep alike.** The client fallback and the compositor glass must
  stay visually close. Screenshots cover both.
- **Trusted UI.** It stays compositor-drawn (ADR 0009, COMP-10). Panes give a
  client no new way to imitate a prompt; it can already do that with its own
  pixels.
- **Spec.** COMP-02 §9 and COMP-06 §1 need an Appendix A row recording client
  panes.
