# 0090 — DE mode: a stacking desktop with compositor-drawn chrome
Status: accepted
Date: 2026-11-05
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0062 defined `mode "de"` as hybrid plus desktop icons and pointer-first
navigation. The owner now wants it to be a full KDE-style desktop: windows stack,
with titlebars, maximize, snapping, a window menu, Alt-Tab and an overview.

Three things stand in the way:

- C-20 removed maximize from the whole compositor. Its reason was that maximize
  was a second "almost fullscreen" beside tiling. Under stacking that reason is
  gone.
- No spec section owns compositor-drawn chrome that is not trusted UI. COMP-10
  covers prompts and the emergency panel only, and the COMP-03 §1.1 VERIFY note
  warns against treating every compositor overlay as a trust surface.
- COMP-02 §7 says redaction never applies to the physical output. S-05 §4 and
  Corpus C treat a thumbnail as a pixel operation governed by class. A switcher
  thumbnail on the physical output falls under both.

## Options
1. **Titlebars drawn by abyss; client-side decorated apps keep theirs.** Same as
   KWin. The compositor grows chrome.
2. **Titlebars on every window.** Consistent, but client-side decorated apps show
   two rows of chrome.
3. **A helper client draws the window menu and switcher.** Keeps the compositor
   smaller, but needs a new socket surface, starts slowly, has no reliable
   position, and a client showing thumbnails would need real capture authority.

## Decision
Take option 1, in `mode "de"` only.

- Chrome is drawn by the compositor, outside `trusted_ui/`, before the TrustedUI
  pass, with none of COMP-10's palette or layout. It confers no trust.
- A toplevel's geometry stays the client content rect, and the titlebar sits
  above it in a frame rect, so agent coordinates do not change.
- Maximize returns under `de` only. C-20 stands in `wm` and `hybrid`, and an
  agent's `set_state(maximized)` stays refused everywhere. *(amended
  2026-11-08)* A maximized window looks like a lone tile (gapped, bordered,
  rounded), not a flush square, so it stays clear of the inset taskbar.
- Desktops are the existing ten per-output workspaces, laid out as a grid.
- Thumbnails are built by the compositor and shown only by it. They take the
  stricter reading of the conflict above: redacted like a capture target, even
  though only the human sees them.
- The window menu, switcher, overview and previews are never in a capture pass
  list. Titlebars are, and a redacted window's titlebar is redacted with it.

Recorded as C-24 (COMP-02 §7, §8; COMP-05 §1, §2, §3.1, §7.1; COMP-17 §1, §2;
S-05 Corpus C).

## Consequences
- `render/capture.rs` must add titlebars to its pass list, which is TCB and gets
  an owner line-by-line review.
- Owed tests: a secret window never appears in Alt-Tab, the overview, a hover
  preview or a capture, including its titlebar; a Corpus C case for each thumbnail
  path; `set_state(maximized)` refused under `de`; the Steam startup-maximize
  case stays declined in `wm` and `hybrid`.
- A `secret` window's title still shows on the physical output, as it does in the
  taskbar today.
- Under `de`, a `private` window's thumbnail is shown unredacted apart from secret
  nodes. It reaches no client and records no audit event.
- Spec drift recorded rather than fixed: COMP-05 §2 says workspaces are dynamic,
  and the code has a fixed ten per output. C-24 notes the as-built count; whether
  dynamic workspaces are still wanted stays open.

## Revisit when
- A thumbnail needs to reach a client, which would be capture and would need
  COMP-11 authority.
- S-05's human-side rule for titles of `secret` windows is decided otherwise.
- COMP-10 gains a rule on client text in compositor chrome.
