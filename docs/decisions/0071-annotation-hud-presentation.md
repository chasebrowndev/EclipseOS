# 0071 — Annotation HUD: liquid-glass presentation, owner colours, error kind
Status: accepted
Date: 2026-10-02
Deciders: chase (owner), Claude (advisory)

## Context
The COMP-18 overlay drew an opaque, square, chamfered amber slab in Spleen
8×16, in three golds none of which was the system accent, with nine gold marks
around one answer. It matched neither STYLE.md (blurred glass, radius 13–14,
white text, one accent) nor the rest of the desktop. Failures had no look of
their own and needed a region, so a failure with nothing to point at bracketed
an empty corner. ADR 0054 recorded Spleen and the pick's bar-and-chip marker.

## Options
1. Restyle inside the existing bitmap path: cheapest, but keeps the pixel face
   and leaves failures looking like answers.
2. Instrument Sans for prose: matches STYLE.md's label face, but COMP-18 §2
   requires monospace, so it needs a spec change for little gain.
3. JetBrains Mono from a pre-rasterised atlas, the window glass material,
   owner-set colours, and a bounded `kind` for failures.

## Decision
Option 3. The overlay is a rounded glass panel (the window material: blur, rim,
shadow) with white text and the accent spent only on the pick. Its face is
JetBrains Mono, smooth-rendered from an 8-bit alpha atlas generated offline and
checked in beside its generator, with no runtime font dependency (ADR 0009
holds: no toolkit). Trusted UI keeps `font.rs` and its own face (COMP-10 §2),
so the two surfaces never share a typeface — this widens, not narrows, the
COMP-18 §1.1 distinction. Every overlay colour comes from the owner's
`annotations { … }` block in `abyss.kdl`; a caller cannot name one. A caller may
choose `kind: "answer" | "error"` and may omit the rectangle (unanchored,
placed top-centre by the compositor, no pick). Amends COMP-18 §1.3, §2, §3,
§3.1 (Appendix G-01..G-04) and supersedes ADR 0054's face and pick-bar notes.

## Consequences
- The annotation art cache key grows by colours, kind and fade state; nothing
  is rebuilt per frame.
- The panel-tint alpha has a floor (.50) so white text stays legible on white
  pages; theming can tint, not erase, the panel.
- `annotation_create` rejects an unknown `kind` and a pick without a
  rectangle (fail closed).
- Regenerating the atlas is a script run, reviewed like any asset.
- `annotation.rs` keeps ADR 0040's owner line-by-line review.

## Revisit when
A caller needs a third look, or the compositor gains a real text shaper.
