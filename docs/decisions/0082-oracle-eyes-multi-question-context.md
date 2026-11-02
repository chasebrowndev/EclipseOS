# 0082 — Oracle Eyes: multi-question screens, rolling context, model and image modes
Status: proposed
Date: 2026-11-01
Deciders: chase (owner), Claude (advisory)

Amends 0054 (one pick becomes a list). Oracle Eyes is an add-on (ADR 0066), not part of the shipped OS.

## Context
Quiz-style screens defeated the one-frame, one-question, text-only design:
- a screen may hold several questions, each with its own options and "(Choose N)";
- a question or its passage may span several screens (scrolling);
- OCR can be too poor to answer from, with no way to show the model the pixels;
- the model could not be changed without editing config, and auto mode could not recover on its own.

## Decision
1. **Question groups.** `choice::detect` tags every option with a group. The reply is a list of per-group answers, validated per group against detected options. `annotation_create` takes `picks` (max 16, each validated like `pick`); `pick` and `picks` together are refused. Out-of-anchor picks are dropped one by one, as in 0054.
2. **Rolling context.** The daemon keeps a bounded buffer of *redacted OCR text* from recent frames, with tail-overlap de-dup so scrolling stitches. It is bounded by lines, bytes and TTL, cleared on auto-mode reset, never logged, and sent only as a secondary "earlier on screen" section. This replaces the old "nothing accumulates" rule with "nothing accumulates beyond this buffer".
3. **Model switch.** `oracle-eyes.models` lists model names; `annotation-model-switch` cycles them. Names that start with `-` or contain whitespace are rejected (they are passed to `--model`).
4. **Image mode.** `annotation-image-toggle` / `oracle-eyes.image-mode` sends the frame as an image block. Off by default, and visible in the answer detail when on. Pixels bypass OCR redaction, which is why it is opt-in.
5. **Auto-escalation.** In auto mode a weak answer (low confidence, poor or cut-off OCR) is retried once with the next model, and with image mode only if `image-auto-escalate` is true (default false).
6. All six chords ship bound (select `Super+grave`, auto-toggle `Super+a`, expand `Super+Alt+Up`, dismiss `Super+Alt+Down`, model-switch `Super+Alt+Right`, image-toggle `Super+Alt+Left`). A chord only takes its key while its add-on hook is on; otherwise the key is forwarded, so a stock install without the add-on loses nothing.

## Consequences
- Ratchet and fail-closed rules are unchanged: nothing here grants a capability; capture still needs its policy grant.
- Image mode and image escalation widen what leaves the machine; both are off by default and need an explicit opt-in.
- The text-only replay corpus (749 local screenshots) cannot measure layout or dark-theme fixes.
