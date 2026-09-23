# 0054 — Annotations gain a title and a pick marker
Status: accepted
Date: 2026-09-22
Deciders: chase (owner), Claude (advisory)

## Context
Oracle-Eyes answers questions about what is on screen. A large share of those
are multiple choice, and the useful answer there is not a sentence but *which
option* — shown on the option itself. `Oracle-Eyes/PROPOSED_FEATURES.md`
parked this over three questions:

1. **Q1.** A highlight aimed by the model's reply is on-screen geometry chosen,
   indirectly, by whoever wrote the page. COMP-18 §1.3 says a caller may affect
   nothing but the glyphs.
2. **Q2.** Who finds the options: the compositor or the addon?
3. **Q3.** What happens when detection or the reply is wrong?

Separately, COMP-18 §2 strips newlines, so the panel had no way to carry a
headline apart from its body.

## Options
For the marker:
1. **Text only** — the panel says "B". No new surface, but the user still has to
   find B on screen; that is the whole problem.
2. **Caller-drawn** — the caller sends a second anchor for the option. Adds
   nothing a caller could not already do with a second overlay, and nothing
   ties the two together visually.
3. **A compositor-drawn pick, constrained to lie inside the anchor.**

## Decision
Option 3, answering the three questions as follows.

**Q1 — bounded, not eliminated.** `annotation_create` takes an optional
`pick {x, y, w, h, label}`. The compositor drops a pick that is not wholly
inside the (clamped) anchor, and draws it only inside the part of the anchor it
is prepared to draw (`drawable_region`). A caller therefore gains exactly one
choice: *which sub-rectangle of a region it could already bracket* gets marked.
The label is at most two ASCII alphanumerics. The marker is visibly distinct
from the brackets — a faint wash, a solid bar and an inverted label chip that
reappears in the panel header — and, like everything in the pass, is an
untrusted claim below trusted UI and absent from capture. The marker's
furniture is anchored to the pick, not confined to it: the wash overhangs the
pick by 2 px, and the bar and chip sit to its left, so a pick flush with the
anchor's left edge puts them up to 35 px outside the anchor. That overhang is a
fixed shape in compositor colours at a compositor-chosen offset; the caller
still chooses only where within its own region the pick sits.

**Q2 — the addon.** Option detection is local and geometric, in Oracle-Eyes
(`choice.rs`). The compositor never learns to parse screen content. The model
may name only a label the detector found; the rectangle comes from OCR, never
from the reply. Text on the page claiming an answer is framed to the model as
part of the specimen.

**Q3 — degrade to prose.** No detected options, an unknown label, or a
malformed reply means no pick is sent; the answer is shown as text. A pick the
compositor rejects leaves the overlay drawn without it.

**Titles.** `annotation_create`/`update` take an optional `title`, reduced to
one capped line and drawn as the panel header. The compositor still chooses
all styling.

Placement and styling changed in the same pass, under the existing "compositor
owns presentation" rule and with no protocol surface: four candidate positions
around the anchor (right, below, left, above) scored by distance to the pick or
region, clear of the anchor and of other overlays (COMP-18 §1.3's collision
avoidance, previously unimplemented); a width chosen per output from a fixed
column range; the Spleen 8x16 bitmap face (BSD-2-Clause, vendored as data
under ADR 0009's no-toolkit rule) in place of the 5x7 face at 3x.

## Consequences
- COMP-18 §1.3 and §3 amended (Draft v0.2, §3.1).
- A compromised or injected caller can now make the marker land on the wrong
  option within its own region. That was already possible in words ("the
  answer is D"); the marker makes it more persuasive, which is why it must stay
  visibly the addon's claim and never borrow trusted UI's look.
- `annotation_update` cannot move a pick. Changing the pick is a new overlay,
  so a live overlay's marked geometry is fixed for its lifetime.
