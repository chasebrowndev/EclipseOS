# 0128 — Trusted prompts may be glass, with a blur floor and opaque text wells
Status: proposed
Date: 2026-11-18
Deciders: chase (owner), Claude (advisory)

## Context
COMP-10 prompts are compositor-drawn so that a client cannot imitate one (threat T9, ADR 0009). `trusted_ui/modal.rs` draws the panel as a CPU pixel buffer with pre-composited colours and a test, `the_panel_is_opaque`, that fails on any translucent pixel outside the corner cut. No spec text or ADR requires opacity (COMP-10 §5 only fixes layout and forbids theming); the rule lives in that test and in the comment on `Colors` ("nothing here depends on what is behind the panel"). The owner wants prompts to share the glass material of the rest of the shell (STYLE.md, COMP-02 §9).

What a translucent panel adds: a client drawing under the prompt could place content that reads as part of it (a fake "Allow" label, a fake secret) or shape the contrast of the real text, and the anti-spoofing phrase's legibility would depend on client pixels. This is our inference; the spec does not name it.

Unspecified today: blur behind trusted UI, and what a blurred prompt does under capture and lock. The prompt is not captured (COMP-02 §4, "TrustedUI last, always") and is not drawn under the session lock (`trusted_ui/mod.rs`: it stays pending and returns on unlock).

## Options
1. Keep the panel opaque; give it a glass-style edge only (rim, shadow, lozenge buttons). No new risk. Does not match the rest of the shell.
2. Frosted look from an opaque snapshot fill. Looks like glass without sampling live client pixels, but needs a second render path just for prompts.
3. Glass body sampled from the compositor's own lower passes, with a mandatory blur floor and tint floor so no client content is legible, and every text-bearing area on an opaque or near-opaque well. Same blur machinery as the rest of glass.

## Decision
Option 3. The prompt panel is a front-most glass member whose backdrop is everything the compositor has already drawn for that output, sampled only from the live output pass. Rules, each enforced by a test:
- **Blur floor.** The prompt always blurs through the full down/up chain at no fewer than the configured maximum passes for its preset (never fewer than 6), and never samples the sharp half. No config, theme, rule or client can lower it. Test: a 1 px checker and a text-like pattern behind the panel leave at most a small, fixed local contrast (threshold set in the test and documented) in the body pixels.
- **Tint floor.** The render call raises any tint alpha to `PROMPT_MIN_TINT_ALPHA = 0.25` with a 6-pass chain, which keeps the worst measured leak (1.74 of 6 levels) well under the bound. Each palette then sets its own ground alpha (.40 in both) as a launcher-like glass; text on the body is held to 4.5:1 over a mid scene only (`text_reads_on_glass_over_a_mid_scene`), and reads less well over pure black or white wallpapers, by design (the dim scrim caps the brightest scene at `1 - 0.45` = 0.55; the scrim is lowered from 0.6 to 0.45 so the glass is visible). Both from the two built-in palettes only (ADR 0084); not themeable.
- **Opaque controls.** The personal-secret frame (with the UNSPOOFED marker), the untrusted block, entry fields and buttons stay opaque (alpha 255), as does the rim ring. The heading, body, facts and warning text sit directly on the glass body, which the blur and tint floors bound: a new test asserts each of those colours holds 4.5:1 against the worst ground the tint can produce. `the_panel_is_opaque` becomes `the_fallback_panel_is_opaque` (the panel drawn when the blur fails) plus `the_glass_panel_keeps_its_wells_and_controls_opaque`.
- **Unchanged.** Last in the pass order, never in a capture target (`the_capture_pass_cannot_see_trusted_ui`), not drawn and not holding the seat under the session lock, fixed layout, only the two compiled-in palettes, the 0.6 dim scrim behind it.
- **Failure mode.** If the glass program fails to compile or the blur chain cannot be built for a frame, the prompt falls back to the opaque panel, never to an unblurred translucent one.

## Mechanism
Today `trusted_ui::elements` returns `[panel texture, dim scrim, commit cards]` front-to-back and the backends (`winit.rs`, `drm.rs`, `headless.rs`) splice them in front of everything `collect_elements` produced, never into a capture target (`render/scan_tests.rs` checks the call site). The panel is a CPU-rasterised RGBA buffer (`modal::rasterize`).

The glass body is added without moving that splice point:
1. `ec-abyss-render` gets one new entry point, `prompt_backdrop(renderer, store, output, behind, rect, radius, light)`, that returns a `BlurElement` for the panel rect. `behind` is the scene the backend already holds plus the scrim and cards (everything behind the panel), so the blur samples only the compositor's own already-drawn passes. It builds a private one-member tier keyed `BlurKey::Prompt`, never shared with windows.
2. The backend inserts that element directly behind the panel texture and in front of the scrim. `trusted_ui` stays free of blur internals: it exposes the panel rect and the palette (`ui.glass_rect(output)`, `Colors::glass_tint`) and the backend calls the render crate.
3. `modal::rasterize` draws the panel ground with the palette's tint at the floor alpha (translucent) and the text wells, entry field, secret frame, marker and button labels at alpha 255. The corner cut stays.
4. The floors are constants in `trusted_ui` (`PROMPT_MIN_PASSES = 6`, `PROMPT_MIN_TINT_ALPHA` per scheme) passed into the render call; the render call clamps `passes = max(passes, PROMPT_MIN_PASSES)`, forces the Frost preset (no sharp half, no lensing, no dispersion) and ignores `decoration.blur.*`, window rules and every theme value.
5. If `ensure_programs` or the chain build fails, the render call returns `None` and the backend draws the existing opaque panel texture (`rasterize(.., opaque = true)`), so a failure is never a see-through prompt.

Tests: `the_text_wells_are_opaque`; a leak test that renders a 1 px checker and a text-like pattern behind the panel and bounds the local contrast of body pixels; `prompt_glass_ignores_config` (passes/tint independent of `decoration.blur`); `prompt_falls_back_opaque_on_failure`; the capture and lock tests unchanged.

## Consequences
Easier: prompts match the shell's glass; one blur implementation. Harder: the prompt render path now depends on the blur store (a new TCB-to-render edge) and the GPU; the blur floor and tint floor are new invariants that tests must keep. Forbidden: any config or client influence on prompt tint, blur or alpha. We owe: the tests above, the COMP-10 §5 amendment (dated note, VOL1 and the VOL2 A-08 index), the `docs/STYLE.md` line on prompt palettes, a note under `docs/decisions/0084-themes.md` as a reference only, and owner line-by-line review of every changed line under `trusted_ui/` (F-07 §4; the `tcb-review` job flags it).

## Revisit when
A client-pixel leak through the blur is found (any measured contrast above the floor), the blur chain changes shape (passes, formats), prompts are drawn under the lock, or the capture rules for trusted UI change.
