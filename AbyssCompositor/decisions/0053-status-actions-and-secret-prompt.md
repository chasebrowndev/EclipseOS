# 0053 — The status service gains actions; secrets go through their own prompt process
Status: accepted
Date: 2026-09-21
Deciders: chase (owner), Claude (advisory)

## Context
`eclipse-services::status` was a read-only feed: battery, network and
bluetooth state went in over D-Bus and came out as `Update`s. The taskbar's wifi
and bluetooth drawers could show state but not change it, so a user could not
join a network or pair a device without a terminal. PROPOSEDFEATURES recorded
this as blocked on two questions:

1. Where do the actions run? Scanning, connecting and pairing are D-Bus calls
   that can block for seconds, and nothing on a view's draw path may block.
2. Where does the passphrase go? A `password`-role value is never delivered,
   logged or stored (root invariant), and trusted UI is compositor-drawn, never
   a layer-shell client. So the secret cannot simply be typed into the taskbar.

## Options
For the secret:
1. **A compositor-drawn trusted prompt.** This is the strongest isolation. But
   COMP-10 §3 is a closed list of authority surfaces (capability grants,
   override, emergency). A wifi passphrase grants nothing the compositor
   enforces, so adding it would stretch the TCB for something that is not an
   authority decision.
2. **Hand off to an external picker** (nm-applet, iwgtk). This means no
   control over the look, a second toolkit, and the modularity goal (ADR 0052)
   gives nothing back for it.
3. **A separate small client process whose whole surface is `secret`.**

## Decision
**Actions.** `status` gains an action handle alongside the feed. Every action
runs on the service's own runtime and reports its result back as an `Update`
over the same channel. None of them runs on a draw path. The wifi path talks to
NetworkManager over D-Bus directly, and bluetooth talks to BlueZ, including an
`org.bluez.Agent1` for pairing. Neither shells out. Actions are for the
human-driven UI process only.

**Secrets.** Option 3. `eclipse-secret-prompt` is its own crate and binary
(ADR 0052). It shows one password field, hands the value straight to the
service action, overwrites it, and exits. The taskbar starts it with the network
or device to prompt for, but never sees the secret. S-05 §2 wants
authentication surfaces classified `secret` as a whole surface. The app cannot
declare that itself until COMP-09 lands, so the shipped `/etc/eclipse/policy.kdl`
does it with an owner rule on the exact app-id:
`windowrule "sensitivity secret" { app-id "^eclipse-secret-prompt$"; }`.

This is compliant: the `password`-role rule governs what the compositor,
the semantic tree and audit deliver (COMP-09 §1/§3, S-04 §2). An application
holding its own text input is outside it.

The prompt is an **xdg_toplevel, not a layer-shell surface**, because windowrules
(and so sensitivity) only match toplevels. It has a fixed size, and abyss floats
fixed-size toplevels (min_size == max_size) by default, so no rule is needed in
the deliberately empty shipped `abyss.kdl`.

## Consequences
- Today the `secret` class buys capture redaction and no direct scanout. Agent
  and clipboard gating on the prompt arrive with COMP-08/09 and need no change
  here.
- `status/mod.rs` no longer describes a read-only module. Its doc states the
  rule instead: actions are not called from a draw path.
- Anyone replacing the taskbar can reuse the prompt: the prompt takes the
  target network or device as arguments and needs nothing from hyperion.
- Floating fixed-size toplevels by default affects every client, not just the
  prompt. That matches other tiling compositors, and an explicit `tile` rule
  still overrides it.
- **One pairing agent per session.** BlueZ has one default agent, and hyperion
  runs one bar per monitor. So the supervisor process, which has no surface,
  registers `org.bluez.Agent1` and starts the prompt for PIN and passkey
  requests, for yes/no confirmation, and to show a code. The bars send the pair
  action but never act as the agent. A displayed pairing code is not a secret,
  so it reaches the prompt on argv.
- The tray host (`eclipse-services::tray`) shares this shape. It is a
  StatusNotifierWatcher and host on the service's own runtime, and it reports
  items and menus back as updates. Activate and menu clicks are actions.
