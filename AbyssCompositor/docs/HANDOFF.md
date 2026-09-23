<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Session handoff

## State as of 2026-09-23

Earlier handoffs are archived under `docs/handoff/` (`2026-09-11.md` is the
long-running one this file used to be; `2026-09-19-greeter-to-abyss.md` is
resolved). `docs/STATUS.md` is the progress record; this file is only "where
are we, what next".

### Merged to `main` (through PR #31)

- **Phase 1 compositor**: milestones 1–9c done; 9d landed (tree source owed,
  M22); 9e partial (`class_source`, `irreversible_capable` missing); 9f
  harness exists with no subjects. M3/M4/M6 gates want hardware.
- **Phase 2 start**: `policyd` + `policy-eval` (M10). No `protocols/agent/`,
  `trusted_ui/`, `policy/` or `audit/` in `abyss` yet.
- **DE userland**: `hyperion` (taskbar, split out in #28), toasts, center,
  launcher, settings, policy viewer, `eclipse-secret-prompt`, and
  `eclipse-services` (notifications, SNI tray, wifi/BT/battery status and
  actions, session control, the `eclipse-screensaver` bridge).
- **Look**: rounded glass with blur on by default, layer surfaces blurred, the
  panes' radius synced from `decoration.rounding` (#29); shadow cache (#30).
- **COMP-18 / Oracle-Eyes**: compositor-drawn annotations and region selector
  in `render/`, multiple-choice picks with a pick marker (#31, ADR 0054).
- **Distribution**: split PKGBUILD, signed tailnet repo, archiso medium;
  EclipseOS installed on the Framework 13 on 2026-09-19 and logs in to Abyss
  from greetd. The dev host now runs Abyss as its session.

### Not merged

- **PR #32, `bar-live-updates`** (open): touchpad swipe gestures, touch and
  tablet input (`31785c3`); only floating windows honour
  `set_maximized`, and a maximize from an unmapped tiled window is ignored
  (`a006d2e`, `20d8e3a`); hyperion wakes on compositor events and follows
  `title` changes (`429040a`).
- Local `main` has `aa8a4f7` (keep a tiled window tiled on a client maximize
  request) that is not on `origin/main`. Reconcile it with #32, which fixes
  the same thing.

### Where to look next

1. **`docs/PROPOSEDFEATURES.md` → "Code still to write"** — the Phase 1 exit
   and Phase 2 backlog, each item with spec, location and milestone. The
   code-only Phase 1 items are 9e's two fields, ext-image-copy cursor capture,
   per-device input settings, the COMP-17 `mode` key and the taskbar clock.
2. **`docs/KNOWNBUGS.md`** — PKG-03 (`eclipse-secret-prompt` is not packaged
   or installed, so wifi/BT secrets fail on an installed system) is the one
   with user impact. BLUR-02 needs re-checking after #29 before anyone works
   on it; so does the ghost-window entry in PROPOSEDFEATURES (after
   `8f76207`).
3. **Manual gates** — three monitors on a real TTY (M3), Firefox/mpv on KMS
   with the bench budgets (M4), a logind suspend/lid cycle (M6), then 14 days
   as the only compositor.
