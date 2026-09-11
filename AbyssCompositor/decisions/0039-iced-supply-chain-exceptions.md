# 0039 — Supply-chain exceptions for the iced toolkit
Status: accepted
Date: 2026-09-11
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0038 settled that the EclipseOS userland is native Rust on iced 0.14 with
`iced_layershell`, rather than Quickshell/QML. Adding the first crate that
depends on iced (`eclipse-ui`) pulls in wgpu and cosmic-text, and with them
four things the `cargo deny` gate rejects:

- `paste 1.0.15` — RUSTSEC-2024-0436, unmaintained.
- `ttf-parser 0.25.1` — RUSTSEC-2026-0192, unmaintained.
- `clipboard-win 5.4.1` and `error-code 3.4.0` — BSL-1.0, not in `allow`.

None of these is a vulnerability. Three of the four are never compiled on our
target at all: `paste` arrives through `metal` (macOS-only) under wgpu-hal, and
the two BSL-1.0 crates through `window_clipboard` under iced_winit on the
`cfg(windows)` branch. Only `ttf-parser` is real code in a shipped Linux
binary — it is the font parser inside fontdb/cosmic-text, which is how iced
draws every glyph.

The gate is deliberately strict (ADR 0035): unmaintained fails the build. That
strictness is worth keeping, so the question is whether to loosen it, replace
the toolkit, or vendor around the dependency.

## Options
1. **Narrow, justified exceptions.** Two `advisories.ignore` entries and two
   per-crate `licenses.exceptions`, each with a written reason and a revisit
   trigger. Pros: keeps the gate strict everywhere else; the exceptions are
   legible and expire on a concrete event. Cons: the advisory ignores are
   blanket for those crate ids — if one later becomes a vulnerability under a
   *new* advisory id, that new id still fails, but the ignored one would not.
2. **Allow BSL-1.0 repo-wide.** Simpler. Cons: decides a licence question for
   every future crate on the strength of two Windows crates we never build.
3. **Drop iced.** Reopens ADR 0038 over two unmaintained transitive crates,
   and the named fallback (sctk + tiny-skia + parley) has its own font parser
   with its own advisory surface. Not proportionate.

## Decision
Option 1. `paste` and `ttf-parser` are ignored by advisory id with the reason
and the revisit trigger written next to each; BSL-1.0 is allowed for exactly
`clipboard-win` and `error-code` and for nothing else. A licence we accept for
a crate that is never compiled on our target is not a licence we have decided
to accept in general, and the config now says so.

The vendored typefaces in `assets/fonts/` are OFL-1.1 data assets, not crates;
cargo-deny does not see them and the OFL text ships beside each family.

## Consequences
- The gate stays green with the toolkit in the tree, and stays fail-closed for
  everything not named here.
- We owe a re-check whenever the iced major version moves: these four entries
  are pinned to the reasons above, not to iced in general.
- `ttf-parser` is the one entry with a real runtime surface. It parses faces we
  vendor ourselves, never attacker-supplied font files — if a pane ever loads a
  font from disk at the user's direction, that changes and this ADR is wrong.

## Revisit when
cosmic-text moves off `ttf-parser`; wgpu drops `metal`'s use of `paste`; either
advisory is upgraded from unmaintained to a vulnerability; or the iced major
version changes.
