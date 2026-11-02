# 0083 — Annotation HUD draws Latin and Greek
Status: proposed
Date: 2026-11-01
Deciders: chase (owner), Claude (advisory)

Amends 0071 (the HUD atlas repertoire). Spec: COMP-18 §2, G-06.

## Context
Oracle Eyes answers quiz questions, which often carry accented Latin, Greek letters and maths symbols (π, Σ, √, ≤). The HUD drew only printable ASCII; every other character became `?`.

## Options
1. **Runtime font engine.** Draws anything, but breaks ADR 0009 and adds compositor attack surface.
2. **Widen the HUD atlas only.** Regenerate the pre-rasterised JetBrains Mono atlas offline with a larger fixed set.
3. **Widen both atlases.** Also opens homoglyph spoofing (Greek or Cyrillic lookalikes) in trusted prompts.

## Decision
Option 2. `assets/fonts/hud/gen_atlas.c` now emits format HUD2: 97 base cells plus 322 extended (Latin-1, Latin Extended-A, Greek, selected maths, punctuation, super/subscripts), 419 in all. The generator fails if a face lacks a codepoint; U+0132, U+0133, U+2071-2073 are absent from JetBrains Mono and U+00AD is excluded. Atlases grow from about 257 KB to about 1.1 MB.
`text::sanitize_annotation` keeps exactly the atlas set. It strips controls, bidi, zero-width and combining characters (COMP-18 §2 says stripped, not `?`), and turns anything else into `?`, one per character. Pick labels stay ASCII (COMP-18 §3.1, ADR 0054). `text::sanitize` and the trusted UI face are unchanged.
OCR is unchanged: `ocr-lang` stays `eng`. A trial with `eng+ell` on the owner's screenshots turned Latin letters into Greek lookalikes (`p1` read as `Ρ`) and put Greek accents on plain English words, so Greek OCR is not shipped; a user may still pin `ocr-lang` by hand.

## Consequences
- No new dependency and no runtime font code.
- The trusted UI still cannot be spoofed with lookalike letters.
- The trusted `sanitize` still maps C1 and bidi to `?`, not strips them; a follow-up if the rule should be uniform.
- Greek-labelled options (`α)`) are body text, not pick targets.
