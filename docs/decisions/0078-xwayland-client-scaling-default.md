# 0078 — XWayland defaults to client scaling
Status: accepted
Date: 2026-10-13
Deciders: chase (owner), Claude (advisory)

## Context
COMP-07 §4 made compositor scaling the default: X11 clients render at scale 1
and abyss upscales them, "blurry but always correct". Client scaling was the
opt-in (`xwayland { scaling "client" }`; it renders correctly at fractional
scale since #119).

On the owner's 1.5x panel the default made every X11 app blurry. That
includes Steam and every Proton game, which are the X11 clients this machine
actually runs. Once X11 fullscreen started working (#120), a game filled the
screen with one upscaled 1x image. The owner's rule: if a default causes a
problem out of the box, the default is wrong.

## Options
1. **Keep compositor scaling as the default.** Always the right size. Blurry
   on any non-integer or >1 scale, which is exactly where it is noticed.
2. **Default to client scaling.** Steam, Proton games and DPI-aware toolkits
   (GTK3+, Qt5+, Electron) are sharp. Old DPI-unaware apps (Java/Swing
   without `GDK_SCALE`, GTK2, bare Xlib) render small. KDE Plasma 6 made the
   same choice.
3. **Per-app rule (the spec's original shape).** Best of both, but no rule
   exists yet, and the default still has to be one of the above.

## Decision
Option 2. `xwayland.scaling` defaults to `"client"`; `"compositor"` remains
available. At scale 1.0 the two modes are identical, so only scaled outputs
are affected.

## Consequences
- COMP-07 §4 and the COMP-07 summary in Vol 1 are amended. `docs/CONFIG.md`
  and the schema default are updated.
- DPI-unaware X11 apps render small on scaled outputs until a user switches
  to `"compositor"`. That is the visible cost, and it is documented in
  CONFIG.md.
- Mixed-scale multi-output still takes one scale when Xwayland starts
  (XWL-01 in `KNOWNBUGS.md`). This is unchanged and now affects the default.

## Revisit when
A per-app `xwayland-scaling` rule lands. The default can then go back to
whichever mode the compat matrix favours, with rules for the exceptions. Also
revisit if Xwayland's native `-scale` (COMP-07 §7 decision 1) becomes
reliable.
