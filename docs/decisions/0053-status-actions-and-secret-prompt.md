# 0053 — The status service gains actions; secrets go through their own prompt process
Status: accepted; amended by 0066 (the pairing agent moves out of hyperion to `ec-pairing`)
and by the 2026-11-15 amendment below (the Terminal target)
Date: 2026-09-21
Deciders: chase (owner), Claude (advisory)

## Context
`ec-services::status` was a read-only feed: battery, network and
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

**Secrets.** Option 3. `ec-secret-prompt` is its own crate and binary
(ADR 0052). It shows one password field, hands the value straight to the
service action, overwrites it, and exits. The taskbar starts it with the network
or device to prompt for, but never sees the secret. S-05 §2 wants
authentication surfaces classified `secret` as a whole surface. The app cannot
declare that itself until COMP-09 lands, so the shipped `/etc/eclipse/policy.kdl`
does it with an owner rule on the exact app-id:
`windowrule "sensitivity secret" { app-id "^ec-secret-prompt$"; }`.

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
- The tray host (`ec-services::tray`) shares this shape. It is a
  StatusNotifierWatcher and host on the service's own runtime, and it reports
  items and menus back as updates. Activate and menu clicks are actions.

## Amendment (2026-11-15): the Terminal target

Owner decision. A password a program asks for from a terminal pane (`sudo -A`,
ssh through `apoth ssh`) goes to `ec-secret-prompt`, under a new **Terminal**
target, when Apotheosis runs on Abyss. The wire and the checks are T-22 §3.

**Option 3, extended.** This is the option chosen above: a separate client
process whose whole surface is `secret`. Terminal is a fourth target beside
wifi, bluetooth and API key. It is not a new option. The app-id stays
`ec-secret-prompt`, so the owner windowrule covers it unchanged. Option 1 (a
compositor prompt) stays rejected for this use, and more firmly: COMP-10 §3.14
forbids a compositor prompt's secret from ever going to the requester, and a
terminal password has to reach the program that asked for it.

**Apotheosis is a relay, not a recipient.** The value goes from the field to
`ec-secret-prompt`'s stdout pipe, then to Apotheosis, then over the armed askpass
socket to `apoth askpass`, and from there over ssh's or sudo's own pipe to the
program that verifies it. That program is the verifying authority. The E-04
reading of the root invariant ("delivered" means delivered to anything except
the verifying authority) holds. Each relay hop carries the value once, keeps
no copy, wipes every buffer it owns, never logs it and never shows it in
`Debug`. The semantic tree, audit and the compositor never see it.

**Input.** `ec-secret-prompt terminal` takes no other argument. The request
context comes on stdin as one JSON object, and the answer goes out on stdout.
Argv is never used, because argv is world-readable in `/proc`. Apotheosis is the
only intended spawner, and it spawns the prompt directly so that it is the
prompt's parent.

**Verified and unverified blocks.** The request carries two blocks:
- *Verified*: facts Apotheosis read from the kernel. These are the window, tab
  and pane titles; whether the requester is in the pane's foreground process
  group (`tcgetpgrp`); its ancestry chain to the pane; its `exe` link; and its
  real and effective uid.
- *Unverified*: what the program says about itself. These are its argv, its cwd,
  the prompt text and, for `apoth ssh`, the host it named.

The prompt shows the verified block as verified only after checking that its
own parent is a live Apotheosis. The check is a pidfd on the parent, the
parent's `/proc/<ppid>/exe` equal to the installed `/usr/bin/apotheosis` (in
debug builds, an `apotheosis` beside the prompt's own binary also passes), and
the pidfd still not readable after the `exe` link was read. If the check fails,
the prompt shows the same facts as unverified, with a plain note that they did
not come from Apotheosis. Argv is never persisted or audited.

**Spoofing, accepted.** A client prompt can be imitated. Any process of the same
uid can spawn `ec-secret-prompt terminal`, or draw a lookalike window. The
parent check stops a forged "verified" block. It cannot stop a lookalike, and
neither can anything else a same-uid client could use. The owner accepts this
for terminal passwords, `sudo` included. D-07 §6 was narrowed the same day to
match: its "never a client prompt" rule covers polkit `auth_admin` only.

**Masking.** Each typed character shows as one `*`. The value itself is never
drawn, measured for display or published.

**Focus.** The prompt opens with keyboard focus in its field, because the user's
own command asked for it. Abyss focuses a newly mapped toplevel, and the prompt
focuses its field when it opens. Nothing opens it on an unrelated event. There
are two triggers. One is an askpass request from a process in a pane. The other
is the T-17 §5 auto-open: a getpass-style read (ADR 0115) in the focused pane by
a command the user just ran, whose answer is typed into the pane. A
click-to-enter path was considered and dropped, because it costs more clicks
than plain `sudo`.

**Off Abyss**, or when `ec-secret-prompt` is not installed, Apotheosis's
in-window askpass sheet answers as before. Host-key questions, confirmations
and notices (`SSH_ASKPASS_PROMPT=confirm|none`) also stay on the sheet. Only a
secret is routed.
