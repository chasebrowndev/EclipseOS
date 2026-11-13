# 0108 — The compositor owns the lid; logind only suspends
Status: accepted
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
Two things acted on a laptop lid. abyss, per output `lid-close` (COMP-01 §4.1):
`off` turned the internal panel off and moved its windows when another output
was on. logind, with its stock `HandleLidSwitch=suspend` and
`HandleLidSwitchDocked=ignore`, decided on its own whether the machine counted as
docked.

The two disagreed at the greeter-to-session handoff. The greeter's compositor
exits and turns every output off before the session's compositor starts. For
that moment logind sees a closed lid and no external display, and it suspends.
This happened on a docked hybrid laptop (lid closed, two external monitors) on
2026-11-12. The machine suspended under a second after login. On resume the
USB-C DisplayPort link failed to retrain and fell back to 1024x768, so the
monitor stayed dark. The session started while it did not yet hold DRM master,
and the bar never picked up the outputs that reappeared.

COMP-01 §12 open decision 5 (the lid default with an external display) was still
only proposed.

## Options
1. **Keep both handlers** and try to shorten the handoff gap. The gap cannot be
   closed: between two compositors, no one drives the outputs.
2. **abyss holds logind's `handle-lid-switch` inhibitor.** That does not cover the
   gap either: the inhibitor dies with the greeter's compositor.
3. **logind ignores the lid; abyss decides and asks logind to suspend.** One
   handler, which sees the outputs it is about to light.
4. **Replace logind.** Rejected. logind also provides seat and device access
   (libseat), sessions for polkit, the power actions with their inhibitors, and
   the `org.freedesktop.login1` API that apps call.

## Decision
Option 3. `eclipseos-abyss` ships `HandleLidSwitch=ignore`,
`HandleLidSwitchExternalPower=ignore` and `HandleLidSwitchDocked=ignore` in
its existing logind drop-in (`packaging/logind/10-eclipseos.conf`, installed to
`/usr/lib/systemd/logind.conf.d/`, so an admin can override it in `/etc`).
abyss is the only lid handler, and suspend is still performed by logind through
`systemctl suspend`.

This settles COMP-01 §12 open decision 5. `lid-close "off"` (the default) works as
follows:
- **Lid closes, another output on:** the internal panel turns off and its windows
  move to another output.
- **Lid closes, no other output on:** the machine suspends.
- **Lid closed and the last other output goes away:** the machine suspends instead
  of lighting the internal panel.
- **Lid opens:** the internal panel turns back on.

`suspend` always suspends. `ignore` does nothing.

## Consequences
- A docked laptop no longer sleeps at login, and the closed-lid rules have one
  owner.
- This amends ADR 0087's "Lid `suspend` stays". The login screen still sleeps on
  lid close, because the greeter runs on abyss. With no compositor running at all
  (a crashed session or a bare TTY), closing the lid does nothing. The owner
  accepted that: a bare TTY is never the intended userspace.
- The live medium keeps its own `do-not-suspend.conf`.
- Owed: the COMP-16 M6 hardware gate, a real lid and suspend cycle docked and
  undocked, which has never been run.

## Revisit when
- abyss gains a supervised handoff that keeps outputs driven between the greeter
  and the session.
- A user needs lid suspend without any compositor running.
