<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# D-07 — First-run setup and agent onboarding

Status: **written 2026-09-24**, specification only. Nothing here is built.
Depends on D-03 (written), D-05 (written).

Governing specs: Tier 6 D-07. Constrained by CHARTER §4 ("Configurable without
a text editor", terminal/GUI parity), COMP-17 §2.1 (setup profiles), COMP-13
§1.3–§1.5 (file split, write API, parity), COMP-10 §2 (personal phrase) and
§3.9 (policy editor), COMP-03 §1.1 (visible region), ADR 0060.

## 1. Shape

The installer (D-03 §4) asks **only** what it needs to put a bootable system on
disk: target disk, hostname, user, passwords. Everything that shapes the desktop
is asked **once, on first login, inside the session**, by `eclipse-setup`: the
profile, the interaction mode, every replaceable component, applications,
appearance, displays, the personal phrase, and agents.

The reason is seamlessness. A choice made in a TTY before the desktop exists is
a choice made blind. In the session, choosing the interaction mode or a bar
shows it happening, and the user leaves setup already standing in the result.

**Setup is a seed, not a layer.** It writes ordinary values into `abyss.kdl`
through COMP-13 §1.4 and installs packages. Afterwards the files are the only
source of truth. Nothing reads "the profile" at runtime. Re-running setup starts
from the current files, not from the profile table.

## 2. What the installer lays down

The installer pacstraps `eclipseos-base`: the compositor, `policyd`, the
greeter set, the D-Bus services D-01 §1 names, `foot`, and **every D-05
component plus `eclipse-setup`**. This is the setup floor: whatever profile is
picked, the wizard's own surfaces and the default candidates for every slot are
already on disk, so the common path (Standard with defaults) installs nothing
further and works offline.

What the floor does **not** carry is profile-specific payload: Full's
applications, alternative components (Quickshell, waybar, …), and the Agentic
stack. Those are installed by setup (§6).

D-03's "what you booted is what you install" holds: the live medium and
`eclipseos-base` stay one list.

## 3. When it runs

`eclipse-setup.service` is a user unit, `PartOf=graphical-session.target`,
linked into `abyss-session.target.wants/` like D-05 §5's units. It starts on
every session and **exits at once** unless `setup.complete` is `#false` or
absent in `abyss.kdl`.

That flag lives in `abyss.kdl` on purpose. COMP-13 §1.5 names "wizard progress"
as exactly the GUI-only state that breaks parity. Setup keeps no store of its
own, and a partially completed run is resumable because every completed step
has already been written (§5).

`eclipse-ctl setup reset` sets `setup.complete #false`, and the next session
runs setup again. The settings GUI exposes the same key ("Run setup again").

## 4. Steps

One full-screen `xdg_toplevel`, keyboard-first, back/next, every step skippable
except where noted. Styled per `docs/STYLE.md`. Each step writes when the user
moves past it, not at the end.

| # | Step | Writes | Notes |
|---|---|---|---|
| 1 | **Keyboard** | `input.kb-layout`, `input.kb-variant` | First, because every later step types. Live: the layout changes on selection. |
| 2 | **Network** | nothing (NetworkManager owns it) | Reuses `eclipse-secret-prompt` for the passphrase. Skippable. Offline disables the package-installing choices in steps 5, 6 and 10 with "needs network" rather than hiding them. |
| 3 | **Profile** | `setup.profile` | Four cards, COMP-17 §2.1. Picking one preselects every later step. Picking another before leaving step 3 re-seeds; after that, a slot the user has touched is never re-seeded. Mandatory. |
| 4 | **Interaction mode** | `mode` | `wm` or `de`, preselected by the profile. Applied live on selection (COMP-17 §2), so the user sees the session change shape before committing. |
| 5 | **Desktop components** | `components.*` | One row per component slot (§4.1). Candidates not on disk show their download size. |
| 6 | **Applications** | nothing in config; package set only | Terminal, browser, office, image editor, file manager, media player. The terminal choice also writes `misc.terminal-command`. |
| 7 | **Appearance** | `general.layout`, `decoration.rounding`, `decoration.blur.enabled`, `animations.enabled`, `bar.position` | Live preview: the session behind the wizard is the preview. |
| 8 | **Displays** | via `calibrate_output` | The COMP-03 §1.1 overscan offer, one output at a time. *Offered, never forced*: "Does the picture fit this screen?" with Yes skipping. The overlay is compositor-drawn (DP-1). |
| 9 | **Security phrase** | nothing (§4.2) | COMP-10 §2 makes this mandatory at first run. It may be deferred, but not silently: a deferred phrase leaves the prompts' "anti-spoofing unconfigured" warning in place, and the review step says so. |
| 10 | **Agents** | see §4.3 | Shown pre-enabled for Agentic, collapsed ("Set up agents") for the others. |
| 11 | **Review** | `setup.complete #true` | Every slot with its value, packages to add and remove, total download size. Apply (§6). |

Steps 1–4 and 7 take effect live. Steps 5, 6 and 10 are staged until the review
step, because they install packages.

### 4.1 Component slots

A slot is a replaceable piece of the desktop with a list of candidates, one of
which may be `none`. Candidates come from a root-owned catalog,
`/usr/share/eclipse/setup/catalog.kdl`, shipped in `eclipseos-base`. Each entry
names the candidate id, its packages, and the command or user unit that starts
it. **The wizard never takes a package name from anywhere but the catalog.**

| Slot | Candidates (initial) |
|---|---|
| bar | `hyperion`, `waybar`, `quickshell`, `none` |
| launcher | `eclipse-launcher`, `fuzzel`, `none` |
| notifications | `eclipse-toasts`, `mako`, `none` |
| control center | `eclipse-center`, `none` |
| terminal | `foot`, `kitty`, `alacritty` (`cataclysm` when P-04 exists) |
| browser | `firefox`, `chromium`, `none` |
| office | `libreoffice-fresh`, `none` |
| image editor | `gimp`, `krita`, `none` |
| file manager | `nautilus`, `thunar`, `yazi`, `none` |
| media player | `mpv`, `vlc`, `none` |

Component slots (bar through control center) write `components.<slot>
"<candidate>"` in `abyss.kdl`. `abyss-session` starts what `components` names,
replacing today's fixed links in `abyss-session.target.wants/` (D-05 §5). A
candidate the catalog does not know is a startup error naming the key, per
COMP-13 §1.2. Application slots write nothing: an application is an installed
package, not a setting.

Choosing a non-native bar is a real choice with a real cost, and the wizard
says so in one line: hyperion's drawers, tray lanes and Settings deep links do
not exist in waybar or Quickshell.

### 4.2 The personal phrase is not typed into the wizard

`eclipse-setup` is an ordinary client (§7). If it collected the phrase, a
client would know it, and COMP-10 §2's guarantee ("nothing outside the
compositor knows it") would be void from the first boot.

So step 9 is a **handoff**. The wizard explains what the phrase is for (the
COMP-10 §2 wording rules apply: recognisable, not sensitive, will appear on
screen during sharing) and asks the user to press `agent-attention`
(SUPER+space). The compositor recognises that no phrase is set and draws its own
phrase-entry surface **first**, on the human seat with a keyboard grab. Once a
phrase is saved it **continues into whatever `agent-attention` normally opens**
(Appendix C open decision 5), so while the phrase is unset the chord's real
target is delayed by one screen, never made unreachable. The wizard learns only
a boolean, from a `phrase {set}` event on the control socket. It never sees
the text.

The first phrase entry is the one trusted surface that cannot show the phrase,
so the chord is its only proof of origin. The surface says so in fixed text:
"This screen only ever appears after you press SUPER+space." The `phrase {set}`
event reaches every owner-uid subscriber, so a same-user client can learn that
anti-spoofing is unconfigured. That is accepted: the prompts' own "unconfigured"
warning already shows it on screen.

The wizard **cannot summon** that surface: COMP-10 §3.9 makes the chord the only
invocation path, so that a client drawing a convincing "set your phrase" button
harvests nothing. This is gated on milestone 15. Until then step 9 reads "not
available in this build" and the review step records it as deferred.

### 4.3 Agents

Step 10 has two independent parts.

**Stack.** Whether to install and enable the agent stack (`policyd` is in the
floor; `agentd`, `brokerd`, the egress proxy and `cataclysm` are not). Default
on for Agentic, off otherwise. Until Phase 2 delivers them (COMP-16 M11–M25),
the Agentic card is marked **preview**, and the stack row lists only what exists.

**Policy starting point.** Two options, and the wizard offers both:

- **Locked down** (default, every profile): `policy.kdl` stays at the shipped
  fail-closed defaults. Every capability an agent later needs is granted by the
  user through a trusted-UI prompt when it is first asked for.
- **Curated preset** (opt-in, Agentic only): a starting grant set shipped as
  `/usr/share/eclipse/policy-presets/agentic.kdl`.

The wizard cannot write `policy.kdl` (COMP-13 §1.3). Choosing the curated preset
therefore does not apply it, and **it does not stage it either**. Nothing about
the preset crosses the control socket. The wizard only tells the user to press
`agent-attention` and pick "Load a starting point" in the **policy editor**
(COMP-10 §3.9). The editor offers that choice itself and reads the preset from
the fixed root-owned path above, never from a path or content a client
supplied. The preset then appears as a pending change: shown as a diff against the current file, each rule with its scope,
accepted or struck individually, then committed as the editor commits any
edit, with the personal phrase on screen. A preset is a set of suggested
grants the human confirms in trusted UI, not authority conferred by a profile.
No-ambient-authority holds. The preset path is gated on milestone 15, like
§4.2.

**Agent onboarding proper**, meaning the first agent's manifest review, provider
credentials and budgets, is out of D-07's reach until A-01/A-02, I-02 and I-07
exist. When they do, it lands as a sub-step of step 10. Provider credentials are
secrets and go to the secret store, never to `abyss.kdl`.

## 5. Resuming and failure

Each step writes as it is left, so power loss mid-setup resumes at the first
step whose key is unset. A write refused by COMP-13 §1.4 validation is shown
with its `file:line:col` error and the step does not advance. Nothing is
half-applied.

A package transaction that fails at review leaves `setup.complete` false and
the `components.*` values for the failed slots unwritten. The review step
reports which packages failed and offers retry or "continue without".

## 6. Installing packages

Setup needs root for exactly one thing: the package transaction at review.
`eclipse-setup-helper` is a small root binary reached through polkit, action
`org.eclipse.setup.apply`, **`auth_admin`** (never `_keep`: setup runs one
transaction, and a keep window would let any same-session process ride the
authorisation), **active local session only**. Agent sandboxes must not reach
the system bus at all. That is S-03's to guarantee, and until S-03 says so, it
is an assumption this section depends on. The helper takes **catalog candidate
ids, not package names**, and resolves them against the root-owned catalog. It
then runs one `pacman -S --needed` (and, for removals the user explicitly
ticked, one `pacman -Rns`) against the configured, signed repositories
(D-02 §5), then `systemctl enable` for exactly the **system units the
catalog entry lists** (the agent stack's `agentd`, `brokerd` and proxy units).
An id not in the catalog is refused. The helper writes no config and takes no
unit name from its caller.

Removal is opt-in ("Uninstall components you are not using"). By default, a
floor component set to `none` stays installed and simply is not started.

## 7. Trust

`eclipse-setup` is **not TCB**. It is an ordinary client of the COMP-13 §1.4
write API, like the settings GUI (COMP-17 §3), and holds to the same limits:
scoped to `abyss.kdl`, no private store, no path into `policy.kdl`.

The two security-relevant steps, the phrase and the policy preset, are **handed
off to compositor-drawn surfaces summoned by chord**, and the wizard learns
only whether they completed. The package helper is root, but its input
language is a closed list of catalog ids. It is the one new privileged component
and small enough to review line by line. **When its crate lands, its path is
added to `gate.yml`'s `tcb-review` paths in the same PR**, so that review is
enforced, not just promised.

## 8. Relationship to the §7 gates

CHARTER §7's "fresh install from ISO to working agent session in under 30
minutes" is measured through this flow: D-03 §4 plus D-07 with the Agentic
profile. B-02's companion gate, *a user can reach a working configuration
without editing a file*, is met by D-07 alone for every profile, and is the
acceptance test for it.

## 9. Test plan (into COMP-15)

- Each profile, accepted with no changes, produces exactly the COMP-17 §2.1
  table in `abyss.kdl` and the matching package set.
- Standard with defaults completes offline and installs nothing.
- Killing the session at every step and logging back in resumes at that step
  with earlier values intact.
- `eclipse-setup` has no code path that writes `policy.kdl` and never receives
  the phrase text. Assert on the socket traffic.
- `eclipse-setup-helper` refuses an id not in the catalog, a caller not in the
  active local session, and any argument that is a package name.
- `eclipse-ctl setup reset` followed by a re-run preselects the current files'
  values, not the profile's.

## 10. Open decisions

1. **Where the phrase is stored** is Appendix C open decision 3, unchanged.
   §4.2 needs only that the compositor can tell "set" from "unset".
2. **Full's application list** beyond the slot defaults in COMP-17 §2.1.
3. **Candidate catalog governance**: which alternatives ship in the catalog,
   and whether AUR-only ones (Quickshell is in `extra` today, some are not) are
   admissible. Proposed: official repositories and `[eclipseos]` only.
4. **Locale and timezone** stay installer questions (D-03 §4 step 6) or move
   here. Proposed: move timezone here (it has a map), keep locale at install.
5. **The curated Agentic preset's contents.** Written once S-01's grant set
   exists. Until then the option is shown disabled.
