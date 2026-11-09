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

## Amendment: composed cells
The format stays HUD2; the repertoire grows from 419 to 430 cells. Eleven private-use cells, U+E000..U+E00A, are composed glyphs: a base letter with a combining mark over it. p̂ x̂ β̂ σ̂ θ̂ μ̂ λ̂ ρ̂ take U+0302; x̄ p̄ μ̄ take U+0304. `gen_atlas.c` rasterises the base, then the mark from the same face, centred on the advance box and lifted clear of tall bases. `text::sanitize_annotation` folds a listed base followed by its mark into one glyph, which counts as one character for wrap and width. It replaces every caller-supplied private-use codepoint with `?`, so no caller can name a composed cell. Other combining marks are still stripped. y with circumflex or macron is not composed: U+0177 is already a cell, and U+0233 is absent from the atlas (adding it needs a sorted insert and a format bump). The trusted `sanitize` is unchanged.

## Amendment: title symbols
The repertoire grows from 430 to 435 cells: U+25CB, U+25CF, U+2713, U+2717 and U+2736, ahead of the composed tail (their codepoints are unchanged). U+2736 is one frame of the window-title spinner Claude Code uses; its siblings U+2722, U+2733, U+273B and U+273D, and U+2714 and U+2605, are absent from JetBrains Mono and nothing is substituted. The compositor's title display (`titlebar::display_title`, also the window menu and overview labels) now drops characters the atlas cannot draw instead of showing `?`; the annotation and trusted sanitisers still show `?`.
