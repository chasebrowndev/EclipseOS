# 0060 — Setup profiles and in-session first-run setup
Status: accepted
Date: 2026-09-24
Deciders: chase (owner), Claude (advisory)

## Context
EclipseOS should be modular (any bar, any launcher, any app set) and still
easy to set up. COMP-17 had one preset concept, the runtime `mode "wm"|"de"`,
and it also decided the autostart set. D-07 (first run) was an unwritten
index row. The installer (D-03 §4) is a TTY script that already asks for disk,
hostname and user.

The owner wants four profiles (Full, Standard, Minimal, Agentic), one picked
during setup as defaults, with every value then refinable individually.

## Options
1. **Profiles replace `mode`.** One preset concept. But tiling vs desktop is a
   runtime interaction style users flip, not an install decision.
2. **Profiles as a live default layer** under explicit config. Switching
   profiles later re-flows every untouched value, at the cost of a third
   source for every value, and "why is this set" is no longer answerable from
   the file (CHARTER §4).
3. **Profile picked in the installer.** Simple to build, but the user chooses
   a mode and a bar blind, in a TTY, before any of it exists.
4. **Profiles as a one-time seed, chosen in-session on first login**, setting
   defaults for `mode`, component slots, apps and the agent stack.

For Agentic's policy: (a) locked down only, (b) a looser preset applied by the
profile, (c) both offered, the preset opt-in and applied only through the
trusted policy editor.

## Decision
Option 4 with policy option (c). The installer asks only disk, hostname, user
and passwords. On first login `eclipse-setup` asks for the profile, then the
mode, then each slot, with the profile's choices preselected and applied live
where possible. The profile writes plain `abyss.kdl` values and is not read
again. `setup.profile` is a record, not a layer. The autostart set moves from
`mode` to a new `components {}` block. Agentic defaults to the same locked-down
policy as every profile. A curated preset is opt-in and reaches `policy.kdl`
only through the COMP-10 §3.9 editor, summoned by chord, rule by rule. The
personal phrase is entered on a compositor-drawn surface by the same chord,
never in the wizard.

## Consequences
- COMP-17 §2.1/§2.2 (v0.2), COMP-10 §2 (DA-03), D-07 written, D-03 §4 and D-05
  amended. Appendix D records it.
- Owed code: `components {}` in the schema and `abyss-session` starting from it
  (replacing the fixed `.wants/` links); `eclipse-setup` (frontend agent);
  `eclipse-setup-helper` + polkit action (`auth_admin`, small root binary
  taking catalog ids only, added to `gate.yml`'s `tcb-review` paths when it
  lands); the candidate catalog; `eclipse-ctl setup reset`; the
  `eclipseos-base` floor in `dist/`.
- The phrase and preset steps are gated on milestone 15. Until then they show
  as deferred, and prompts keep the "anti-spoofing unconfigured" warning.
- Forbidden: the wizard writing or staging `policy.kdl` content, the wizard seeing the phrase, the
  helper accepting a package name.

## Revisit when
S-01's grant set exists (the preset can be written), or a second installer
target makes offline Full/Agentic installs a requirement.
