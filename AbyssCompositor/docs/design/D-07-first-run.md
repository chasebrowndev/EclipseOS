<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# D-07 — First boot: the graphical installer and setup

Status: **written 2026-09-24**, specification only. Nothing here is built.
Depends on D-03 (written), D-05 (written).

Governing specs: Tier 6 D-07. Constrained by CHARTER §4 ("Configurable without
a text editor", terminal/GUI parity), COMP-17 §2.1 (setup profiles), COMP-13
§1.3–§1.5 (file split, write API, parity), COMP-10 §2 (personal phrase) and
§3.9 (policy editor), COMP-03 §1.1 (visible region), D-01 §5 (seat model),
ADR 0060.

## 1. Shape

**First boot is booting the ISO.** The live medium starts an abyss session and
runs one full-screen program, `eclipse-setup --install`, that takes a machine
from blank to a configured desktop: language and keyboard, network, disk,
identity, profile, interaction mode, every replaceable component, applications,
appearance, displays, agents, then the install itself. There is no TTY
questionnaire and no second wizard after reboot. The installed system boots to
the greeter and then to the desktop the user just built.

Doing it in the live session is what makes it seamless. The live medium *is*
the desktop, so choosing a mode, a bar or a layout changes what is on screen
right now, and the user commits to something they have already seen.

**Setup is a seed, not a layer.** What the user chooses becomes ordinary values
in `abyss.kdl` and a package set. Afterwards the files are the only source of
truth. Nothing reads "the profile" at runtime (COMP-17 §2.1). The same program
runs again on an installed system as `eclipse-setup --reconfigure`, which skips
the install-only steps (§4) and starts from the current files, not from the
profile table.

## 2. The medium

### 2.1 What boots

The live medium gains a session, where D-03 §5 currently has only a root shell
on tty1. `greetd` autologins an unprivileged `liveuser` into `abyss-session`,
which autostarts `eclipse-setup --install`. `liveuser` follows D-01 §5 like any
user: no groups, a logind session on the seat, nothing else. Explicitly **no**
sudoers or wheel entry, **no** sshd, and no other listening service on the
medium, so nothing reaches `liveuser` from the network (the polkit rule in §6
also requires an active local subject). The root shell on
tty2 and `/root/install-eclipseos.sh` stay (§8): a live session that will not
come up on some hardware must not strand the user.

The installer is started by the live medium's own greetd session command
(`abyss-session` plus a live-only user unit under `airootfs`), **not by any key
in a config file**, so no exec-style key exists in either `abyss.kdl` to carry
over. The user's `abyss.kdl` is empty at boot and receives only what the user
chooses; §5 enforces what may cross.

### 2.2 What the installer lays down

The installer installs `eclipseos-base`: the compositor, `policyd`, the greeter
set, the D-Bus services D-01 §1 names, `foot`, **every D-05 component** and
`eclipse-setup`. This is the setup floor: it is what D-03 §2.1 already says,
"what you booted is what you install". Today `build-iso.sh` bakes the signed
`[eclipseos]` packages into the medium (`/root/eclipseos-repo`) and
`install-eclipseos.sh` pacstraps from that copy, so **the EclipseOS packages
install offline**. Their dependencies from the Arch repositories (mesa, fonts,
`networkmanager`, …) still come from a mirror, so the floor as a whole does
**not** install offline yet. **Owed:** bake the floor's official-repo closure
into the medium too, or state that step 3 (network) precedes Apply.

What the floor does **not** carry is profile-specific payload: Full's
applications, alternative components (Quickshell, waybar, …) and the Agentic
stack. Those come from the network. The medium cannot hold a browser and an
office suite and stay a sensible size.

## 3. What runs when

| Where | Program | Privilege |
|---|---|---|
| Live session | `eclipse-setup --install` | `liveuser`, unprivileged. An ordinary client, not TCB (§7) |
| Live session | `eclipse-setup-helper` | root, reached through polkit. Repartitions, installs, configures the target (write surface: §6) |
| Installed system | `eclipse-setup --reconfigure` | the user. Launched from Settings ("Run setup again") or `eclipse-ctl setup reset` |
| Installed system | `eclipse-setup-helper` | root, reached through polkit. Packages only (§6) |

`--install` and `--reconfigure` are the same program with the same steps. A
step whose subject is a disk, an identity or a locale is skipped in
`--reconfigure`.

## 4. Steps

One full-screen `xdg_toplevel`, keyboard-first, back/next, every step skippable
except where noted. Styled per `docs/STYLE.md`. Steps that change the running
session apply live, so the live medium itself is the preview.

| # | Step | `--install` | Effect |
|---|---|---|---|
| 0 | **Welcome** | yes | `eclipse-welcome`: the eclipse animation, greetings in ten languages, "press Space". Space begins step 1. Not a setup step: it writes nothing, and `--reconfigure` skips it. Reduced motion is honoured |
| 1 | **Language, keyboard** | yes | `input.kb-layout`, `input.kb-variant` (live); locale for the target. First, because every later step types |
| 2 | **Timezone** | yes | Target timezone; `hwclock --systohc` at install |
| 3 | **Network** | both | NetworkManager owns it. Reuses `eclipse-secret-prompt` for the passphrase. Skippable. Offline disables the network-dependent choices in steps 8, 9 and 12 with "needs network" rather than hiding them |
| 4 | **Disk** | yes | §4.2. Destructive; confirmed at review, not here |
| 5 | **Identity** | yes | Hostname, username, passwords. §4.3 |
| 6 | **Profile** | both | `setup.profile`. Four cards (COMP-17 §2.1). Picking one preselects every later step. Mandatory |
| 7 | **Interaction mode** | both | `mode`. Live: the session changes shape as the user chooses |
| 8 | **Desktop components** | both | `components.*` (§4.1) |
| 9 | **Applications** | both | Terminal, browser, office, image editor, file manager, media player. The terminal also writes `misc.terminal-command` |
| 10 | **Appearance** | both | `general.layout`, `decoration.rounding`, `decoration.blur.enabled`, `animations.enabled`, `bar.position`. Live preview |
| 11 | **Displays** | both | The COMP-03 §1.1 overscan offer, one output at a time, via `calibrate_output`. *Offered, never forced*; the overlay is compositor-drawn (DP-1) |
| 12 | **Agents** | both | §4.4. Collapsed ("Set up agents") unless the profile is Agentic |
| 13 | **Review** | both | Every choice, the disk plan in full, packages to add and remove, total download. Apply |
| 14 | **Install** | yes | Progress; then "remove the medium and restart" |

Steps 1, 7, 8 (once its candidates are on disk), 10 and 11 apply live. Steps 4,
5, 9, 12 and 13 stage until Apply.

Each live step writes its `abyss.kdl` value through COMP-13 §1.4 as the user
leaves it. Wizard progress is therefore in the config file and nowhere else
(COMP-13 §1.5), and `setup.complete` is written `#true` at Apply.

### 4.1 Component slots

A slot is a replaceable piece of the desktop with a list of candidates, one of
which may be `none`. Candidates come from a root-owned catalog,
`/usr/share/eclipse/setup/catalog.kdl`, shipped in `eclipseos-base`. Each entry
names the candidate id, its packages, the command or user unit that starts it,
and any **system** units it needs enabled. **Nothing outside the catalog ever
supplies a package or unit name.**

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
"<candidate>"`. `abyss-session` starts what `components` names, replacing today's
fixed links in `abyss-session.target.wants/` (D-05 §5). A candidate the catalog
does not know is a startup error naming the key (COMP-13 §1.2). Application
slots write nothing: an application is an installed package, not a setting.

A non-native bar is a real choice with a real cost, and the wizard says so in one
line: hyperion's drawers, tray lanes and Settings deep links do not exist in
waybar or Quickshell.

### 4.2 Disk

Version 1 offers what D-03 §4 builds today and nothing more:

- **Erase the whole disk.** GPT, 1 GiB ESP (`ef00`) and the rest as ext4 root.
  No LUKS, no btrfs subvolumes, no swap (D-03 §7). Those wait on D-04, and the
  step names them as "not yet" rather than omitting them.

Not offered yet: install alongside another OS, use existing partitions, custom
layout. The step says so plainly. They are open decisions (§10).

Guards, all enforced by the helper and none trusted from the UI:

- UEFI only: refuse unless `/sys/firmware/efi` exists (D-03 §4 step 1).
- The disk the live medium booted from is never offered, and is refused if named.
- The disk is addressed by its `/dev/disk/by-id` path, chosen from the helper's
  own listing, never typed as a free path.
- The step shows model, size, and every existing partition with its filesystem
  and label. Review repeats them under the words "everything on this disk will
  be erased", and **requires the disk's name typed again** before Apply is
  enabled. This is D-03 §4 step 2's rule, carried over.

### 4.3 Identity

Hostname and username are validated against fixed patterns by the helper.
Passwords are entered into a `password`-role field, sent to the helper on a
file descriptor, and applied with `chpasswd` (§6 rejects newlines and NUL). They
are never written to any file the wizard owns, never logged, never placed on an
argument list. The wizard clears its own buffer once sent; **it cannot promise
the toolkit's widget state, undo history or glyph cache hold no copy**, which is
one reason the wizard is not trusted (§7). Wi-fi credentials from step 3 belong
to the *live* NetworkManager and are **not** carried to the target unless the
user asks, and then only through the secret store, never a file the helper
copies. The user is created with `-G wheel` and nothing else (D-03
§4.2); no legacy device groups.

### 4.4 Agents

Step 12 has two independent parts.

**Stack.** Whether to install and enable the agent stack (`policyd` is in the
floor; `agentd`, `brokerd`, the egress proxy and `cataclysm` are not). Default
on for Agentic, off otherwise. Until Phase 2 delivers them (COMP-16 M11–M25) the
Agentic card is marked **preview** and the row lists only what exists.

**Policy starting point.** Two options, both offered:

- **Locked down** (default, every profile): `policy.kdl` stays at the shipped
  fail-closed defaults. Every capability an agent later needs is granted by the
  user through a trusted-UI prompt when first asked for.
- **Curated preset** (opt-in, Agentic only): a starting grant set shipped at
  `/usr/share/eclipse/policy-presets/agentic.kdl`.

The wizard cannot write `policy.kdl` (COMP-13 §1.3), and **it does not stage the
preset either**. Nothing about the preset crosses the control socket. Choosing
it records `setup.pending-preset #true`, a nudge and nothing more, and the
preset is loaded **on the installed system**, at first login, through the
policy editor (§4.5).

**Agent onboarding proper**, meaning the first agent's manifest review, provider
credentials and budgets, is out of D-07's reach until A-01/A-02, I-02 and I-07
exist. Provider credentials are secrets and go to the secret store, never to
`abyss.kdl`.

### 4.5 What is deliberately left for the installed system

Two things are **not** done on the live medium, and this is a decision rather
than a gap: the personal phrase (COMP-10 §2) and the agent policy preset.

Both belong to the *installed* compositor's trust root. A phrase entered on the
live medium would have to be carried across by copying a file a same-user
process could have written, and a policy staged the same way is exactly the
"client stages `policy.kdl`" hole COMP-13 §1.3 exists to close. The live medium
is throwaway, and it should not be the origin of anything the installed system
must trust.

So at first login the compositor itself shows the COMP-10 §2 "anti-spoofing
unconfigured" warning, and if `setup.pending-preset` is set, `eclipse-toasts`
adds a nudge. The user presses `agent-attention` (SUPER+space):

- **Phrase entry** is drawn by the compositor, first, on the human seat with a
  keyboard grab. It cannot show a phrase (none exists), so the chord is its only
  proof of origin, and the surface says so in fixed text: "This screen only
  ever appears after you press SUPER+space." Once a phrase is saved it
  **continues into whatever `agent-attention` normally opens** (Appendix C open
  decision 5), so the chord's real target is delayed by one screen, never made
  unreachable.
- **The preset** is offered inside the policy editor (COMP-10 §3.9) as "Load a
  starting point". The editor reads it from the fixed root-owned path above,
  never from a client-supplied path or content, and shows it as a pending
  change: a diff against the current file, each rule with its scope, accepted or
  struck individually, committed as the editor commits any edit, with the phrase
  on screen. A preset is a set of suggested grants the human confirms in trusted
  UI, not authority conferred by a profile.

Both are gated on milestone 15. Until then, prompts keep the warning, the
install proceeds, and the review step lists them as deferred.

## 5. The seed

At Apply, the helper installs the target and then **writes the new user's
`abyss.kdl` from the live `liveuser`'s**, so what the user previewed is what
they get. That file is written by a same-uid process and is therefore
*untrusted input to a root program*, exactly like a phrase or a policy would be
(§4.5). The helper treats it accordingly:

- **It reads a fixed path derived from the polkit caller's uid** (passwd lookup),
  never from the caller's environment, opens it `O_NOFOLLOW`, reads it **once**,
  validates *those bytes*, and writes *those same bytes*. No re-open, no symlink,
  no swap between check and copy.
- **Key allowlist, not just validation.** COMP-13 §1.2 validation checks schema,
  not intent. The helper keeps only presentation and preference keys
  (`input.kb-*`, `general.layout`, `decoration.*`, `animations.*`, `bar.*`,
  `mode`, `components.*`, `setup.*`) and **drops every command-bearing key**:
  `bind` actions that spawn or exec, `idle.lock-command`, `misc.terminal-command`,
  and any exec-style key added later (an unknown key is dropped, never kept).
  `misc.terminal-command` is **regenerated from the chosen terminal's catalog
  id**, never copied as a string. Default binds come from the shipped defaults;
  user-added binds are re-made on the installed system.
- Only that one file is carried. The output-layout state file is **not** copied;
  displays are re-detected and, if the user calibrated overscan, the values are
  re-offered on first login (its validation coverage under COMP-13 §1.2 is not
  established, so it is not trusted here). **`policy.kdl` is never carried.**
- A file that fails validation is not copied. The target boots on shipped
  defaults and the failure is shown on the final screen as a count of rejected
  keys, **never echoing file content** (an error that quoted a line would turn
  the helper into a reader of whatever the path pointed at).

Live-only wiring lives in the medium's own `/etc/eclipse/abyss.kdl` (§2.1),
which is root-owned and never read for the seed. That separation is a rule the
helper enforces by reading only the user's file, not merely a convention.

## 6. The helper

`eclipse-setup-helper` is the only privileged program. Its input is a
**structured plan**, never a shell command or a path:

- `disk`: a `/dev/disk/by-id` entry from the helper's own listing
- `hostname`, `username`: validated against fixed patterns
- `password`: on a file descriptor passed with the request (never `argv`,
  never the environment), rejected if it contains `\n`, `\r` or NUL (a newline
  would inject a second `user:password` line into `chpasswd`); applied with
  `chpasswd`. It is a `password`-role field, so it is never delivered, logged or
  stored (root invariant 6)
- `locale`, `timezone`: from the tzdata and locale lists
- `candidates`: catalog ids

It resolves candidates against the root-owned catalog and takes **no package
name and no unit name from its caller**. It enables exactly the system units the
catalog entries list.

**Its complete write surface** (the security boundary, so it is one list): the
target disk (partition table, filesystems), `pacstrap` payload, target
`/etc/{hostname,locale.gen,locale.conf,localtime,fstab,pacman.conf,sudoers.d/wheel}`,
the bootloader config, the new user and its password, the catalog-listed
`systemctl enable`s, and the target user's `abyss.kdl` (§5). Nothing else. On a
failed candidate fetch (§8) it rewrites the affected `components.*` value in
that same file to the native default, which is inside that list.

Two polkit actions, so that "wipe a disk" and "install a package" are different
authorities:

- **`org.eclipse.install.apply`**: disk, identity, locale, timezone, the
  install. Its `.policy` defaults are **`no` for `allow_any`, `allow_inactive`
  and `allow_active`**, so it is fail-closed everywhere it is not explicitly
  allowed. The rule allowing it exists **only inside the ISO's `airootfs`**,
  and requires `subject.user == "liveuser" && subject.local && subject.active`,
  so a remote or inactive `liveuser` login cannot qualify. The helper
  independently **refuses the action unless `/run/archiso` exists**, so an
  installed system cannot be pointed at its own disk even if the rule leaked.
  **A typed disk name in a client proves nothing** against a `liveuser` process
  that sends the right string, so the wipe is confirmed on a **compositor-drawn
  trusted prompt** (COMP-10 class "destructive system action", **owed**): before
  repartitioning the helper asks the compositor to show the disk's model, size
  and by-id name and to return allow or deny from the human seat, and it
  proceeds only on allow. The typed name in §4.2 remains as an in-client safety
  against mistakes, not as the authority. The root shell on tty2 is accepted
  for a person at the keyboard; this gate is against *other processes*.
- **`org.eclipse.setup.apply`**: `--reconfigure` on an installed system, one
  `pacman -S --needed` (and one `pacman -Rns` for removals the user ticked)
  against the configured signed repositories (D-02 §5), plus the units the
  catalog lists. **Removal is derived from catalog ids only**, resolved to the
  packages that catalog entry alone owns, never a caller-named package. Floor
  and essential packages (the compositor, `policyd`, `eclipse-setup`, the
  session, the base) are **never removable**, and a `-Rns` whose cascade reaches
  one is refused, not trimmed. **`auth_admin`**, never `_keep`: setup runs one transaction,
  and a keep window would let any same-session process ride the authorisation.
  Active local session only. **The `auth_admin` password is collected by a
compositor-drawn polkit authentication agent** (trusted UI), never by a client
such as `eclipse-secret-prompt`, since it is an admin-equivalent secret and a
client prompt is spoofable. That agent is **owed** (COMP-10).

Agent sandboxes must not reach the system bus at all. That is S-03's to
guarantee, and until S-03 says so it is an assumption this section depends on.

Removal is opt-in ("Uninstall components you are not using"). By default a floor
component set to `none` stays installed and simply is not started.

## 7. Trust

`eclipse-setup` is **not TCB**. It is an ordinary client of the COMP-13 §1.4
write API, like the settings GUI (COMP-17 §3), and holds to the same limits:
scoped to `abyss.kdl`, no private store, no path into `policy.kdl`, never sees
the phrase.

The helper is root and the one new privileged component, and its input language
is closed (§6). It is small enough to review line by line. **When its crate
lands, its path is added to `gate.yml`'s `tcb-review` paths in the same PR**, so
that review is enforced and not merely promised. So are the artefacts that
carry the same authority: the two polkit `.policy` files, the ISO's `.rules`
file, and `catalog.kdl` with the preset files. A rule or catalog entry is as
privileged as the code that consults it.

## 8. Failure

- **The live session will not start** (no working GPU path, a compositor
  crash). The root shell on tty2 and `/root/install-eclipseos.sh` are the
  fallback. The script becomes a thin client of the same helper and the same
  plan format, so it is a second front end and not a second installer, per
  COMP-13 §1.5's principle. Until the helper exists it stays as it is today.
- **A step's write is refused** by COMP-13 §1.4 validation: shown with its
  `file:line:col` error; the step does not advance. Nothing is half-applied.
- **Power loss during setup, before Apply**: the machine reboots into the live
  medium and starts again; nothing on disk was touched.
- **Failure during the install**: the helper stops at the first failed stage and
  reports which one. Repartitioning is the point of no return, so the plan is
  fully validated *before* the disk is touched, and everything that can fail
  without touching it (catalog resolution, network reachability for the chosen
  candidates, free space) is checked first.
- **A package for a chosen candidate cannot be fetched** after repartitioning:
  the install completes on the floor, the failed candidate's `components.*`
  value is written back to the native default, and the final screen says which
  choice fell back and why.

## 9. Relationship to the §7 gates

CHARTER §7's "fresh install from ISO to working agent session in under 30
minutes" is measured through this flow, with the Agentic profile. B-02's
companion gate, *a user can reach a working configuration without editing a
file*, is met by D-07 alone for every profile and is its acceptance test.

## 10. Test plan (into COMP-15)

- Each profile, accepted with no changes, produces exactly the COMP-17 §2.1 row
  in `abyss.kdl` and the matching package set.
- Standard with defaults installs with the network unplugged.
- The helper refuses: a disk not from its own listing; the medium's own disk; a
  hostname or username outside the patterns; a candidate id not in the catalog; a
  package or unit name; `org.eclipse.install.apply` when `/run/archiso` is
  absent.
- `eclipse-setup` has no code path that writes or stages `policy.kdl`, and never
  receives the phrase. Assert on the socket traffic.
- The seed contains exactly the allowlisted keys: a seed containing a spawning
  `bind`, `idle.lock-command` or a `misc.terminal-command` string yields a target
  file without them; `policy.kdl` and the layout file are not carried; a symlink
  or a file swapped between check and copy is refused; a file that fails
  validation is not copied and its content is not echoed.
- `install.apply` is refused for a remote, inactive or non-`liveuser` subject and
  for any subject when `/run/archiso` is absent; it does nothing without the
  compositor's allow.
- Passwords appear in no file, no log, no argument list, no environment. A
  password containing a newline or NUL is refused by the helper.
- Killing the live session at each step touches no disk. Killing the helper after
  repartitioning leaves a state the next run either completes or cleanly erases.
- `eclipse-ctl setup reset`, then a re-run, preselects the current files'
  values, not the profile's.

## 11. Open decisions

1. **Offline install of the floor** (§2.2). The `[eclipseos]` packages are on the
   medium; the official-repo closure is not.
2. **Where the phrase is stored** is Appendix C open decision 3, unchanged.
   §4.5 needs only that the installed compositor can tell "set" from "unset".
3. **Disk layouts beyond whole-disk erase**: dual boot, existing partitions,
   swap, LUKS. LUKS and rollback belong with D-04; the rest are unscoped.
4. **Full's application list** beyond the slot defaults in COMP-17 §2.1.
5. **Catalog governance**: which alternatives ship, and whether AUR-only ones are
   admissible. Proposed: official repositories and `[eclipseos]` only.
6. **The curated Agentic preset's contents.** Written once S-01's grant set
   exists. Until then the option is shown disabled.
7. **Two trusted-UI surfaces owed to COMP-10**: the "destructive system action"
   confirmation (§6) and the compositor-drawn polkit authentication agent (§6).
   Until they exist, the install falls back to the TTY script on tty2, which is
   root-run by a person at the keyboard, and `--reconfigure` package changes are
   not offered.
