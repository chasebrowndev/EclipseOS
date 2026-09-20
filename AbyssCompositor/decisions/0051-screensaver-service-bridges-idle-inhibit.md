# 0051 — An `org.freedesktop.ScreenSaver` service bridges D-Bus idle inhibits to the compositor
Status: accepted
Date: 2026-09-20
Deciders: chase (owner), Claude (advisory)

## Context
`idle.dpms-timeout-seconds` powers the outputs off after a period with no
input. A playing video must hold it off. abyss already honours
`zwp_idle_inhibit_v1`, but Firefox on a compositor with no GNOME or KDE session
does not use it: it asks the session bus (`org.freedesktop.ScreenSaver`, or the
portal, whose GTK backend forwards to the same name). Nothing owned that name,
so `xdg-desktop-portal-gtk` logged "proxy is for the well-known name
org.freedesktop.ScreenSaver without an owner", abyss never saw an inhibitor, and
the display went dark mid-film.

## Options
1. **Implement the D-Bus interface inside abyss.** Puts zbus and a bus
   connection in the TCB, against ADR 0038.
2. **A window rule that inhibits for Firefox.** Keeps the screen on for as long
   as any Firefox window is open, playing or not.
3. **A userland service owns the name and tells abyss over the control socket.**

## Decision
Option 3. `eclipse-screensaver` (crate `eclipse-services`, module `screensaver`)
owns `org.freedesktop.ScreenSaver` on `/org/freedesktop/ScreenSaver` and
`/ScreenSaver`. It keeps one cookie per `Inhibit`, drops a client's cookies when
that client leaves the bus, and calls the new control-socket method
`set_idle_inhibit {inhibit: bool}` whenever "is any cookie held" flips.

`set_idle_inhibit` is a `Kind::Command`, owner-only like every method. The hold
belongs to the calling connection and is released when it disconnects, the same
lifetime rule as annotations (COMP-18 §3). `IdleTracker::inhibited` treats a
held connection like a mapped inhibitor surface, so DPMS and the locker are held
off together.

## Consequences
- A video in any client that uses `org.freedesktop.ScreenSaver` keeps the
  display on, and stops doing so when it pauses, stops or dies.
- The compositor gains one method and no dependency. Nothing here can do more
  to abyss than keep the screen on, which a Wayland client already can.
- A crashed `eclipse-screensaver` releases every hold with its connection: the
  display then times out normally rather than staying on forever.
- Only the connection that took a cookie can release it.
- If another daemon already owns the name, this one exits non-zero and does not
  steal it.
