# 0038 — The DE userland is native Rust, not Quickshell
Status: accepted
Date: 2026-09-11
Deciders: chase (owner), Claude (advisory)

## Context
OS-2 specified the desktop shell in QML, and COMP-17 §6 open decision (1)
proposed Quickshell as the first-cut toolkit for it. That was written when the
only working human-facing surface was the Quickshell config at
`~/.config/quickshell/eclipse/` — 3761 lines of QML outside the repository,
which is still what the owner looks at every day.

Appendix B then added F-01 §4: every setting must be reachable through a GUI,
files stay the source of truth, and the GUI writes only through the COMP-13
§1.4 API, "never a parallel one." That obliges us to build a settings
application, a control centre and a bar in-repo, which turns the toolkit
question from a preference into a dependency decision with a long tail.

COMP-17 §4 requires a reversal of a specified choice to be re-decided
explicitly rather than allowed to drift. This is that decision.

## Options
1. **Keep Quickshell.** Free notification server, tray host, PulseAudio,
   BlueZ, NetworkManager, launcher and clipboard, all already working in the
   owner's own config. Costs: a C++/QML runtime and the whole of Qt in the
   dependency set, a second language and build system in a repository that is
   otherwise Rust, no `cargo deny` coverage over any of it, and the shell's
   behaviour defined by a project we do not control.
2. **Native Rust (iced 0.14 + iced_layershell).** Everything in one language,
   one build, one supply-chain gate. Costs: we write the services Quickshell
   supplied for free.
3. **Wait and decide later.** Rejected outright: B5 cannot start without it,
   and "later" is how OS-2's QML line survived into a repository that has no
   QML in it.

## Decision
The desktop userland is native Rust. Verbatim from the owner: *"I dont like
having reliance on an outside project like quickshell, lets do everything in
house."* COMP-17 §6 open decision (1) is closed by this ADR; OS-2's "QML, not
Rust" is superseded for the shell surface. The toolkit is iced 0.14 with
iced_layershell 0.19.1 (both MIT, both already inside the deny.toml allow
list), with smithay-client-toolkit + tiny-skia + softbuffer + parley named as
the fallback if iced's layer-shell story does not hold up.

This pulls part of Z-01 forward: the in-house shell was always the end state,
and we are paying for it now rather than after a Quickshell-shaped detour.

## Consequences
We now owe, in-repo, every service Quickshell was giving us free:

- a notification server (zbus, `org.freedesktop.Notifications`, ~200 lines —
  no dunst, no mako)
- a StatusNotifierItem tray host (`system-tray`)
- audio (`libpulse-binding`), network, Bluetooth and battery (zbus-xmlgen
  proxies, generated once and committed — not `bluer`/`bluez-async`, which
  pull a second C-linked D-Bus stack)
- session control (`logind-zbus`), a launcher
  (`freedesktop-desktop-entry`), and clipboard (`smithay-clipboard`)

and the widget layer itself: `crates/eclipse-ui`, drawing against
`iced::advanced::Widget`, because the design language in the style spec is not
any toolkit's default. Glass blur is not ours to implement — it comes from
abyss's own `decoration { blur }`.

**This does not move the TCB boundary.** Per COMP-17 §3 these binaries are
ordinary Wayland clients outside the TCB, with no anti-spoofing story and no
authority of their own; they ask the compositor for things over the COMP-13
socket and are refused or obliged like any other client. Trusted UI stays
compositor-drawn (root invariant), and the policy editor stays in milestone 15
for exactly that reason — B6 ships a read-only policy *viewer*, reading
`policy.kdl` from disk, because Policy/Read over the socket stays closed.
That property is what makes this reversal cheap in review terms, and it has to
stay true: an eclipse-ui client that ever needs to be trusted is a signal that
this ADR was applied too widely.

## Revisit when
- iced's layer-shell support breaks or falls behind on a compositor feature we
  need, and the named fallback is no cheaper than Qt.
- Any shell binary acquires a requirement that would place it inside the TCB.
