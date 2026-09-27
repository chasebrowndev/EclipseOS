# 0059 — `gesture "drag"`: modifier + touchpad window drag
Status: accepted
Date: 2026-09-24
Deciders: chase (owner), Claude (advisory)

## Context
On a laptop with no mouse the only way to move a window is a `mousebind`
drag with a physical click held, which is awkward on a touchpad. The owner
wants a held modifier plus a two-finger drag to move the window under the
pointer, with a tiled window taking the Radiant drag (ADR 0058).

COMP-04 §2 as amended by C-03 binds 3/4-finger swipes only. libinput does not
report two fingers as a gesture at all: two-finger motion is finger scroll
(`AxisSource::Finger`), a run of axis events ended by one whose axes are all
zero. Three and four fingers are swipe gestures. So a two-finger drag has to
be taken from the scroll stream, which is where every app's scrolling lives.

## Options
1. **Hardcode Super + two fingers.** No config surface, but the modifier and
   the finger count are a taste question, and there would be no way to turn
   it off.
2. **A `drag` kind on the existing `gesture` node**, alongside `swipe`:
   `gesture "drag" <fingers> "<modifiers>" { move-window; }`. One parser, one
   merge rule, one doc entry.
3. **A `mousebind`-style `touchpadbind` node.** A new collection for one
   binding.

## Decision
Option 2. `fingers` is 2, 3 or 4 and at least one modifier is required, as
with `mousebind`: a bare two-finger drag would take every scroll from every
app. The default is

    gesture "drag" 2 "Super" { move-window; }

A drag for the same finger count replaces the default in place, and
`gesture "drag" <fingers> { none; }` (modifiers optional) removes it. A finger
count is either swiped or dragged, never both, since the begin cannot tell
them apart; whichever node comes second is refused at load and by
`eclipse-ctl config validate`.

Ownership is decided once, at the gesture's first event, and held to its end,
the same rule as a bound swipe: with exactly the modifiers held, not under the
session lock, no other drag or region selection in flight, and a draggable
window under the pointer (the `mousebind` rules: no layer surfaces, popups,
override-redirect windows, maximized or fullscreen windows), the whole gesture
is the compositor's and none of it reaches the app. Otherwise the whole
gesture is the app's, and a modifier pressed mid-scroll does not turn its tail
into a drag. Two fingers are claimed from the scroll stream and ended by its
all-zero stop; three and four from the swipe begin/update/end. Scroll deltas
are flipped back under natural scrolling so the window follows the fingers.

The motion drives the same path as a pointer move (`place_moved`): a Radiant
tile drags over its placeholder with the nearest-centre candidates and
guides and lands on release; a floating window moves, and with Super still
held on release it tiles, exactly as an Alt-drag does (ADR 0058). A cancelled
swipe puts a tile back on its placeholder. The session lock cancels a drag in
flight. No grab is installed: the pointer does not move during the gesture,
so a grab would add nothing. The drag counts for `drag_active`.

Super is the default although ADR 0057 kept Super free for a launcher: a
launcher bound to bare Super fires on a Super tap, which a Super-held
two-finger drag is not.

## Consequences
- Super + two-finger scroll no longer reaches apps over a draggable window.
  Over the desktop, a layer surface or a fullscreen window it still does.
- With the default Super modifier a dragged floating window tiles on release
  unless Super is let go first.
- COMP-04 §2 amended (C-12). `docs/CONFIG.md` documents the `drag` kind.
- Winit's nested backend reports touchpad scroll as `Continuous`, not
  `Finger`, so the two-finger path only runs under the DRM backend; tests
  drive it through a fake input backend.

## Revisit when
A resize or other action is wanted on a drag gesture, or a device reports
two-finger drags as something other than finger scroll.
