# 0056 — Small listed layer surfaces are omitted from capture
Status: accepted
Date: 2026-09-23
Deciders: chase (owner), Claude (advisory)

## Context
The taskbar eye (ADR 0055) shows what Oracle-Eyes is doing. The owner wants
it private: a screenshot or recording must show the plain, static eclipse
ring; only the person at the screen sees the eye. That is the same treatment
the annotation pass already gets (absent from captures by construction,
ADR 0040), but the eye is drawn by hyperion, an ordinary layer-shell client.

## Options
1. **Redact it** as sensitive. The capture shows a black box where the mark
   was — it announces that something is hidden, which is the opposite of the
   static ring the owner asked for.
2. **Draw the eye in the compositor.** Moves taskbar presentation into abyss
   and makes the compositor carry Oracle-Eyes state; ruled out by ADR 0041.
3. **Omit a listed surface from the capture element list.** hyperion always
   draws the plain ring in the bar and draws the live eye on a second, small
   layer surface exactly over it. Captures skip that surface, so the ring
   beneath is what they record.

## Decision
Option 3, as a Policy-owned key in `policy.kdl`:

    capture { hide-layer "hyperion:eclipse-eye" }

`render/capture.rs` skips a layer surface — no elements, no placeholder, no
`Redacted` record — only when all three hold:

- its layer-shell namespace equals the entry's namespace;
- its client's executable basename equals the entry's executable (the
  ADR 0022 stopgap identity already used by `capture.allow`). The namespace is
  the client's own claim and never counts alone;
- it is at most `HIDE_LAYER_MAX` (64) logical px on each side.

The namespace test runs first, so the `/proc` lookup happens only for a
surface already claiming a listed name. A listed surface that fails the other
two is captured normally and logged at `debug`.

## Consequences
- **It is an omission, not a redaction.** `Redacted.rects` is never empty and
  records something a capture was denied; this records nothing. The capture
  is not a faithful copy of the screen for that region, and this ADR is the
  place that says so.
- **The size cap is the security argument.** A surface that can hide from
  every recording could be a fake dialog that leaves no trace. At one icon's
  size it cannot hold one. Raising the cap needs a new ADR.
- **Identity is the stopgap.** Anyone who can run a binary named `hyperion`
  as the owner can use the entry — the same exposure `capture.allow` already
  has until ADR 0022's real client identity lands, and they replace together.
- The default `dist/etc/policy.kdl` lists the eye; removing the line makes it
  appear in captures, nothing else changes.
- Oracle-Eyes never OCRs the eye, a side effect of the same omission.
