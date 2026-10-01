# 0060 — Setup profiles and the graphical ISO installer
Status: accepted
Date: 2026-09-24
Deciders: chase (owner), Claude (advisory)

## Context
EclipseOS should be modular (any bar, any launcher, any app set) and still
easy to set up. COMP-17 had one preset concept, the runtime `mode "wm"|"de"`,
and it also decided the autostart set. D-07 (first run) was an unwritten
index row. The installer (D-03 §4) is a root-only TTY script run by hand.
"First boot" means booting the ISO, so setup has to cover disk configuration
and the install itself, not only desktop choices.

The owner wants four profiles (Full, Standard, Minimal, Agentic), one picked
during setup as defaults, with every value then refinable individually.

## Options
1. **Profiles replace `mode`.** One preset concept. But tiling vs desktop is a
   runtime interaction style users flip, not an install decision.
2. **Profiles as a live default layer** under explicit config. Switching
   profiles later re-flows every untouched value, at the cost of a third
   source for every value, and "why is this set" is no longer answerable from
   the file (CHARTER §4).
3. **Profile picked in the TTY installer.** Simple to build, but the user
   chooses a mode and a bar blind, before any of it exists.
4. **Profiles as a one-time seed, chosen in a graphical installer that runs in
   the live session**, alongside disk, identity and every other choice. The
   live medium is the desktop, so choices are seen before they are committed.
5. **A second wizard after the first login** on the installed system. Rejected
   as the primary path: it splits setup across a reboot and loses the preview.
   It survives as `eclipse-setup --reconfigure`.

For Agentic's policy: (a) locked down only, (b) a looser preset applied by the
profile, (c) both offered, the preset opt-in and applied only through the
trusted policy editor.

## Decision
Option 4 with policy option (c). The live ISO autologins an unprivileged
`liveuser` into an abyss session that runs `eclipse-setup --install`: language,
keyboard, timezone, network, disk (whole-disk erase in v1), identity, profile,
mode, components, apps, appearance, displays, agents, review, install. Choices
apply live where possible. One root helper with two polkit actions does the
work: `org.eclipse.install.apply` (live medium only, passwordless for
`liveuser`, refused unless `/run/archiso` exists) and `org.eclipse.setup.apply`
(installed system, packages only, `auth_admin`). The user's `abyss.kdl` is
validated and copied to the target. The profile writes plain values and is not
read again. `setup.profile` is a record, not a layer. The autostart set moves from
`mode` to a new `components {}` block. Agentic defaults to the same locked-down
policy as every profile. A curated preset is opt-in and reaches `policy.kdl`
only through the COMP-10 §3.9 editor, rule by rule. Neither the preset nor the
personal phrase is set on the live medium and nothing about them is copied to
the target: both are done on the **installed** system at first login, by chord
(SUPER+space), on compositor-drawn surfaces. The live medium is throwaway and
must not be the origin of anything the installed compositor trusts.

## Consequences
- COMP-17 §2.1/§2.2 (v0.2), COMP-10 §2 (DA-03), D-07 written, D-03 §4/§5 and
  D-05 amended. Appendix D records it.
- Owed code: `components {}` in the schema and `abyss-session` starting from it
  (replacing the fixed `.wants/` links); `eclipse-setup` (frontend agent);
  `eclipse-setup-helper` + two polkit actions (small root binary taking a
  structured plan and catalog ids only, added to `gate.yml`'s `tcb-review` paths
  when it lands); the candidate catalog; `eclipse-ctl setup reset`; the
  `eclipseos-base` floor and an on-medium local repo of its closure so the
  floor installs offline; a `liveuser` greetd session on the ISO; the TTY
  script kept as a fallback front end of the same helper.
- The phrase and preset are gated on milestone 15. Until then they show as
  deferred, and prompts keep the "anti-spoofing unconfigured" warning.
- Owed to COMP-10: a compositor-drawn destructive-action confirmation for the
  wipe, and a compositor-drawn polkit auth agent. A client-typed disk name is
  not authority against a same-uid process.
- The seed copy is an allowlist of presentation keys read once from a fixed
  `O_NOFOLLOW` path; command-bearing keys are dropped or regenerated from
  catalog ids.
- v1 disk layout is whole-disk erase only; LUKS, swap, dual boot wait on D-04
  and further decisions.
- Forbidden: setup writing, staging or copying `policy.kdl` or the phrase; setup
  seeing the phrase; the helper accepting a package name, unit name or free
  disk path; the install action running off the live medium.

## Revisit when
S-01's grant set exists (the preset can be written), or a second installer
target makes offline Full/Agentic installs a requirement.
