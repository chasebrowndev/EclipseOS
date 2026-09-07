# 0026 — XWayland is one untrusted trust domain, its clipboard a focus-scoped grant
Status: accepted
Date: 2026-09-07
Deciders: chase (owner), Claude (advisory)

## Context
COMP-07 asks for rootless XWayland so X11 applications run under helios. The X11
protocol has no client isolation worth the name: any client that can open the
display can enumerate every other client's windows, read their titles and
properties, grab the keyboard, take screenshots of the root window, and take
ownership of — or read — every selection. None of that can be fixed from the
compositor side; the X server is a shared bus by design.

That collides head-on with the root invariant "no ambient authority". If X11
windows were treated like Wayland windows, an X11 client would inherit whatever
privileges a Wayland window has *plus* unmediated access to every other X11
window, and the compositor would have handed out an authority it never checked.

The spec does not settle how X11 windows are classified, so this ADR does.

## Options
1. Treat X11 windows exactly like Wayland windows. Simple, and wrong: it grants
   the whole X11 peer-visibility set implicitly.
2. Per-X11-client capability checks. There is nothing to key them on — X clients
   are anonymous to the WM, and the X server itself does the leaking, so the
   compositor cannot enforce a boundary it never sees crossed.
3. One trust domain for all of X11: X11 windows are pinned to the lowest class
   the system offers, and every X-facing capability is granted only under a
   condition the compositor itself observes.

## Decision
Option 3, in three parts.

**Constant class.** `xwayland::security::classify` pins every X11 window to
`sensitivity: private`, `app_trust: standard`, `seat_compat: lock`, and stamps
an `xwayland: true` audit flag. `secret` is refused outright, and a self-declared
class may only tighten — the ratchet rule applied to a channel (X properties)
that is not trustworthy in the first place, so in practice X11 declares nothing.
`seat_compat: lock` means an X11 window is never shared with an agent seat: a
second seat on the X11 domain would expose every other X client to it.

**Focus-scoped selection capability.** `XwmHandler::allow_selection_access` —
the hook every X11 read of a Wayland-owned clipboard or primary selection passes
through — returns true only while an X11 window currently holds keyboard focus.
X has no per-client selection ACL, so the grant is scoped in *time* instead of
by identity: the human's own focus act is the authorization, and the window that
just lost focus loses the clipboard with it. A denial is fail-closed (the X
client sees an empty `SelectionNotify`) and logged without content, per the
"human input is never logged by content" invariant.

**Kill switch.** `xwayland { enable #false }` removes the domain entirely; the
compositor runs unchanged without it. That is COMP-07 §7's open decision 2,
resolved as a config knob defaulting to enabled.

## Consequences
- X11 clipboard reads silently fail when a Wayland window has focus. That is
  visible to the user: paste into an X app after clicking into it, not from a
  script running in the background. Accepted — the alternative is an
  always-open channel out of the human's clipboard into any X client.
- Every X11 window is `private`, so none of them can ever be marked `secret`.
  A password manager that only ships an X11 build cannot get `secret` handling;
  the right fix is a Wayland build, not a hole here.
- Interactive move/resize requests from X11 clients (`_NET_WM_MOVERESIZE`,
  `ConfigureRequest` on a managed window) are ignored: the compositor re-asserts
  its layout rectangle. A tiled window does not talk its way out of the layout.
  Override-redirect windows (menus, tooltips, drag icons) do get what they ask
  for — they are unmanaged by definition — and are tracked separately so a later
  milestone can exclude them from the agent scene.

## Two deviations from COMP-07, both in smithay, not in helios
1. **§1/§6 lazy start.** The spec wants XWayland spawned on the first X11 client
   connection, and asserts no `Xwayland` process exists when no X11 client is
   running. Smithay 0.7's `XWayland::spawn` has no lazy mode: the socket
   preparation it would need (`prepare_x11_sockets`) is private, and hand-rolling
   the listener would drop the first client's pending connection. helios spawns
   eagerly at backend start; the shutdown half of the property holds anyway
   because smithay passes `-terminate`. Costs one idle X server (~10 MB RSS)
   for a session that never runs an X client — or zero, with `enable #false`.
2. **§5 hardening flags.** The spec wants `-noTouchPointerEmulation` and MIT-SHM
   disabled. `XWayland::spawn` hardcodes its argv and exposes no hook for extra
   arguments; only the environment is caller-controlled. Not implemented. The
   fix is upstream (an `args` parameter) or a vendored spawn, and is its own PR.

Both are recorded here rather than silently absorbed: spec and code disagree,
and the bug is in smithay's surface area.

## Revisit when
Smithay gains lazy spawn or an argv hook (deviations 1 and 2 close), or the
policy engine lands and can key a real capability check on X11 window identity
instead of the constant class this ADR pins.
