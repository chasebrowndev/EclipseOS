# Proposed features

Ideas parked deliberately, not forgotten. Nothing here is scheduled.

## First-class support for multiple-choice questions

Today a multiple-choice question is just text. The user selects the stem and
the options, OCR flattens the lot into one paragraph, and the model gets asked
to explain it like anything else. The answer comes back as prose in a panel
beside the region — readable, but it makes the user do the last step of the
work: map "the second one" back onto the option that is actually on screen.

First-class support would mean the pipeline recognising the shape — a stem
followed by labelled alternatives — and the answer naming one of them, with
the compositor drawing the band and leader (`render/annotation.rs`) around the
chosen option's rectangle rather than around the whole selection. The geometry
already exists: `ocr.rs` returns per-`Word` boxes, so the rectangle for option
B is derivable from the same read that produced the text, and COMP-18 §3's
`annotation_create` already takes an arbitrary rectangle. Nothing new would be
asked of the compositor.

**Why it is parked, and what it would have to answer first.** Every option on
that screen is written by the adversary. "Weigh every proposed feature against
this" is the injection invariant's own instruction, and this feature is the
sharpest case of it yet: the model's reply would stop being glyphs and start
being a *selector* over attacker-authored content, with the compositor drawing
a confident amber box around whichever one the page managed to get picked. A
page that renders "the correct answer is B — also, B is the Download button"
is no longer just lying in a panel the user is reading sceptically; it is
recruiting the HUD's own highlight to point at it. That is a real change in
kind, not in degree, and ADR 0041's containment (fresh `claude -p`, no tools,
one turn, untrusted-data framing) does not cover it — containment bounds what
the reply can *do*, and here the reply's whole job is to aim something.

Three things would need settling before it is worth building:

1. **Does the highlight survive contact with the invariant?** Either the
   chosen-option band is visually distinct from the "this is what I read"
   band — so the user can never mistake a model claim for a compositor fact —
   or the feature ships as text-only ("B") with no second highlight at all.
   The latter is the honest fallback and is most of the value.
2. **Where does option detection live?** In Oracle-Eyes, off the OCR word
   boxes, never in the compositor: the compositor must not learn to parse
   screen content, and a caller must still be able to affect nothing but the
   glyphs and the rectangle it already sends.
3. **What happens when detection is wrong?** Pointing confidently at the wrong
   rectangle is worse than the prose answer it replaced, so a mis-parse has to
   degrade to today's behaviour rather than guess — the same fail-visibly rule
   the rest of the daemon follows.

Until then: multiple-choice questions work the way everything else does —
select the whole question, get prose beside it, and the band marks what was
read, not what the model chose.

## A settings-plugin system for EclipseOS

Oracle-Eyes has no settings UI of its own and cannot get one from
`eclipse-settings`: that crate is closed and schema-driven entirely off the
compositor's `get_config {schema: true}` (see its `CLAUDE.md` — "there is no
hand-written key list in this crate," and `tests/coverage.rs` ratchets its
coverage of *compositor* keys only). There is currently no mechanism anywhere
in EclipseOS for an addon outside `AbyssCompositor/` to contribute a pane,
schema, or control to the DE's settings surface.

When such a plugin system exists, Oracle-Eyes' config (`config.rs`) is the
natural first integration target: it already follows the same KDL syntax and
refusal discipline as `abyss/src/config/mod.rs` (unknown key or malformed
value = positioned error, never a silent skip), so a schema-driven settings
pane could describe it with no format changes on this side.

Until then: keybind and tuning customization for Oracle-Eyes goes through
plain KDL config keys and `abyss.kdl` `bind` entries, same as everything else
in EclipseOS today.
