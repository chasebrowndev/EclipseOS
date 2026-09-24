# 0057 — `mousebind`: modifier + mouse-button window move/resize
Status: accepted; amended by 0058 (radiant keeps a dragged tiled window tiled)
Date: 2026-09-24
Deciders: chase (owner), Claude (advisory)

## Context
A floating window could only be repositioned by a fixed 50 px arrow nudge
(`Super+Shift+Arrow`) and could not be resized without client-side
decorations. The owner wants Hyprland's `bindm`: hold a modifier, drag to
move, drag with the other button to resize.

COMP-04 §5 says the human seat carries mouse bindings, and COMP-05 §7 lists
floating move/resize by "bindings, mouse", but neither gives a config syntax.
That is a spec gap, not a conflict; this ADR fills it and no spec amendment is
needed.

## Options
1. **Hardcode Alt+drag.** Simple, but the modifier is a taste question and
   Alt collides with some apps' own Alt+click.
2. **A `mousebind` node**, parallel to `bind` and `gesture`, with defaults.
   One more collection node; the parser and merge rule already exist twice.
3. **Reuse `bind` with a button name as the key.** Overloads keysym parsing
   and the reserved-chord checks for a different device.

## Decision
Option 2:

    mousebind "Alt" "left"  { move-window }
    mousebind "Alt" "right" { resize-window }

`button` is `left`, `right` or `middle`; the action is `move-window` or
`resize-window`. At least one modifier is required — a bare button bind would
take every click from every client. Modifiers are matched exactly, as `bind`
does. A `mousebind` for the same `(mods, button)` replaces the default in
place, the same merge rule as `gesture`. The two lines above are the defaults.

The default modifier is **Alt**, not Super: bare Super is reserved for a
future launcher/drawer, and Super+drag would collide with it.

The press is **swallowed**. `on_pointer_button` starts the compositor-owned
grab (`MoveSurfaceGrab` / `ResizeSurfaceGrab`, no client serial — the human
made the press) before handing the button to the seat; the grab clears pointer
focus, so the client sees neither the press nor its release. Resize drags the
corner nearest the pointer, by quadrant. A tiled window is floated on the
first motion (`place_at`). Layer surfaces, popups, override-redirect X11
windows, maximized and fullscreen windows are not draggable; nothing starts
while another drag, a popup grab or the session lock is up, and then the press
proceeds normally. Injected (virtual-pointer / agent) buttons never trigger
it: those seats carry no bindings (COMP-04 §5).

The floating branch of the `move-*` actions (the 50 px nudge) is removed;
`move-*` keeps swapping tiled neighbours.

## Consequences
- Alt+click no longer reaches apps that use it (some drawing/CAD tools). A
  default can be re-pointed at another action or modifier with a `mousebind`
  node but not removed; add a `none` action if that is missed.
- `docs/CONFIG.md` documents `mousebind`; `tests/config_doc.rs` keeps it honest.
- A floating window has no keyboard move any more. Add one back as its own
  action if it is missed.

## Revisit when
The launcher lands and wants Super for itself, a touch/tablet equivalent is
wanted, or COMP-04 §5 gains a syntax of its own that differs from this one.
