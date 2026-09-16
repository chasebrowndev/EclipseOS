# Desktop profiles — WM mode and DE mode

Proposed 2026-09-10, revised 2026-09-11.

**This is no longer a proposal.** The amendments it was drafting are applied
inline to `ECLIPSEOS_SPECS_v2_VOL1.md` as Appendix B (B-01..B-09), COMP-17
exists as a document, and **DP-1's backend is built and merged**. What remains
here is a work plan against specified behaviour, not a pitch.

Series naming: the work items were renamed **D1–D6 → DP-1–DP-6** to stop
colliding with Tier 6's D-01–D-08 distribution documents.

- **D-0n** — Tier 6 distribution documents (base package set, repo, ISO,
  installer, default userland). See `claude/OS_WORK.md`.
- **DP-n** — the work items below. Mostly compositor code.

## What this is

Abyss gains two presentation modes, selected by config and switchable at
runtime:

- **WM mode** — what abyss is today. Tiling, keyboard-driven, bare. The
  furniture is whatever the user installs.
- **DE mode** — taskbar, desktop icons, a settings GUI, and the rest of what
  someone arriving from KDE or GNOME expects to find already present.

This is a presentation-layer decision inside one compositor. It is **not**
compositor swappability, and it does not touch the agent architecture,
the trust boundary, or any of COMP-02/04/08/09/11.

## Numbering and scope

Phase 2 occupies milestones 10–25 and was already renumbered once by COMP-16
v0.2, so this work takes its own lettered series carrying no ordering
relationship to the numbered milestones. **COMP-17 now exists** in VOL1, as do
COMP-03 §1.1 (per-output visible region), COMP-13 §1.3 (the config file split),
§1.4 (the write API) and §1.5 (terminal/GUI parity). Read those for the
contract; this file is the sequencing.

An earlier draft also proposed a COMP-10 amendment for a settings GUI inside
the trust boundary. **That amendment is withdrawn.** The split below keeps the
settings GUI out of the TCB entirely, and the policy editor that replaces it
belongs to milestone 15's existing trusted-UI work.

**Two of these items are really distribution work.** DP-4 (taskbar) is OS-2 in
`claude/OS_WORK.md`, and DP-6 (desktop icons) is D-05 userland. They are
described here because they are part of what DE mode *means*, but they are not
abyss code.

---

## DP-1 — Per-output visible region (overscan) — **backend done**

Both panels on the development box are Sony TVs at 1360x768 and 1280x720,
which is exactly the hardware where overscan bites.

**The probe ran and answered its question.** `underscan_probe` showed that
nvidia-open exposes none of `underscan`, `underscan hborder` or
`underscan vborder`, so the driver path AMD would have given for free does not
exist here and the compositor implements it. The probe also corrected a
standing misdiagnosis: each connector reports exactly one PREFERRED mode, and
the "ambiguous PREFERRED" bug filed after the first KMS boot was really
duplicate modes in the connector list.

**The implementation chose scale-and-pad, not the letterbox this plan
originally specified.** The scene is scaled down into the inset rectangle and
the margins are left black, so **nothing is ever cropped**. That is the right
answer for a television, where letterboxing would silently discard the edges of
what the compositor drew. B-06 was amended on 2026-09-11 to match the code
rather than the reverse.

**The cost, recorded so nobody rediscovers it as a bug:** scale-and-pad needs a
GLES pass, because NVIDIA primary planes cannot scale — the same constraint
that produced the atomic-commit EINVAL storm fixed in `e024947`. So **an output
with a non-zero inset cannot take direct scanout.** An output at zero inset is
unaffected. When milestone 4's gate goes looking for scanout evidence, measure
on an uncalibrated output.

**What is built** (stub 15 in `STATUS.md`): `outputs/overscan.rs` (per-edge
insets, clamping, the inverse map), `render/overscan.rs` (the scale-and-pad
wrap plus corner markers), the wrap wired into both `backend/drm.rs` and
`backend/winit.rs`, the absolute-pointer inverse in `input/mod.rs`,
`calibrate.rs` as a seat-grabbing state machine, per-panel persistence keyed by
EDID identity with hand-written config winning over saved state, both IPC
methods (`set_output` with `overscan`, `calibrate_output` with
`start`/`commit`/`cancel`), and the `eclipse-ctl output ID overscan` and
`eclipse-ctl output ID calibrate` verbs.

Calibration is compositor-drawn rather than a layer-shell client, because the
overlay grabs the seat and paints in the coordinate space it is remapping. See
the VERIFY note in COMP-03 §1.1: the conclusion is right but the "trusted UI
has to be" justification is probably the wrong reason, and it should be settled
so nobody infers that every compositor-drawn overlay is a trust surface.

**What remains is UI, and it is DP-5:**

- a settings-GUI panel that can start calibration on *any* output the user
  picks, including one plugged in long after first boot;
- a first-boot step that *offers* calibration rather than forcing it.

Until that ships, calibration is reachable only from a shell — which is fine
for the owner and not fine for anyone arriving on a TV.

**The logical-geometry rule still governs everything downstream.** An inset
visible region changes the output's *logical* geometry, so it lives in
`outputs/`, not `render/`, and it flows into layer-shell exclusive zones,
window placement and `shell::arrange`, pointer clamping (COMP-04 §6) and cursor
confinement, fullscreen sizing — a fullscreen window fills the visible region,
never the raw mode — screencopy and `ext_image_copy_capture` geometry, and
`outputs.kdl` persistence.

**Remaining gate:** set an inset on a TV; confirm no window, no layer surface
and no cursor can reach outside it; confirm it survives a compositor restart
and a replug.

## DP-2 — Config file split, then the write API

### The split

Config becomes two files with different trust levels:

- **`abyss.kdl`** — appearance, input, outputs, layouts, keybinds, animations,
  decoration. Writable by an ordinary client over IPC.
- **`policy.kdl`** — the capture allowlist (ADR 0027), the data-control
  allowlist, agent-relevant window rules, and everything COMP-11's enforcement
  table will eventually need. Not writable by an ordinary client, ever.

**Why the split beats a deny list of security-relevant keys:** it is fail-safe
by construction. A new appearance key added next year lands in `abyss.kdl` and
is writable by default, which is correct. A new policy key lands in
`policy.kdl` and is unwritable by default, which is also correct. A key-level
deny list is default-open — forgetting to add an entry silently grants write
access to a security control, and nothing fails loudly enough to notice.

It also buys an option that a single file cannot: `policy.kdl` can be given
different ownership or a different filesystem location, so a sandboxed settings
app cannot reach it at all. That is not a defense against a compromised user
account — abyss and `policyd` both run as the user — but it composes with the
per-agent sandboxing planned in milestone 20, and it costs nothing to preserve.
**Its actual location and ownership are a D-01 decision**, deliberately left
open in COMP-13 §1; settle that alongside the split.

### The hard part: `windowrule` has mixed criticality

`shell/rules.rs` supports one rule construct whose actions span both trust
levels. `float`, `tile`, `workspace N`, `size`, `position`, `output`,
`fullscreen` and `opacity` are appearance. `sensitivity secret|private`,
`app-trust`, `seat-compat` and `no-agent` are security. The construct cannot go
in one file.

**Resolution, now specified in COMP-13 §1.3:** the same `windowrule` syntax is
accepted in both files, but the parser accepts a *different action set* per
file. A `sensitivity secret` action appearing in `abyss.kdl` is refused at
parse time with an error naming `policy.kdl`, exactly as an uncompilable regex
is refused today. This preserves the existing rule that a rule is refused whole
and never applies in part, and it reuses the total-validation machinery from
spec gap 4 rather than inventing a second error path.

The two reserved binds — `agent-override` (Super+Escape) and `agent-attention`
(SUPER+space) — need no special handling. The parser already refuses to let
anyone rebind them in any file.

### Consequences to work through

- **Two inotify watches, two error paths.** `config/watch.rs` watches one file
  today. Both files should behave identically on failure: refuse startup on an
  invalid file (spec gap 4's established behavior), and on a failed reload keep
  the last good content and emit `config-error`. Keeping the last good
  `policy.kdl` *is* the fail-closed behavior — it never widens permissions.
- **Migration.** Existing configs carry capture allowlists and security window
  rules in one file. Given the project's posture of refusing to start rather
  than guessing, the right handling is a precise startup error naming each
  misplaced key and its destination, plus an `eclipse-ctl config migrate`
  command that performs the split once. Not a silent auto-migration.
- **Reads, not just writes.** Whether an untrusted client may *read*
  `policy.kdl` is a separate decision from whether it may write it. Default to
  no. The `SO_PEERCRED` owner-uid gate already limits callers to the owner's
  uid, so this is defense in depth rather than the primary control, but the
  gate table should carry read and write as distinct rows.

### The write API

1. **KDL round-trip editing that preserves comments and formatting.** This is
   the crux risk of the whole plan and deserves a spike before anything is
   promised — it is task 3 of the daily-drive plan, and COMP-13 §1.4 carries a
   VERIFY marker pointing at it. Nobody accepts a settings panel that reformats
   their config or eats their comments because they moved a slider.
2. **IPC methods** — structured `get_config`, `set_config_value`,
   `validate_config`. The caller addresses a key by path; the server resolves
   which file that key belongs to from the schema and checks the caller's gate
   row **for that file**. Every write goes through the same total validation as
   `Config::load`, returning the same `ConfigError` with `file:line:col` and
   the offending token. A write that fails validation leaves the file
   untouched: COMP-13 §1.2's "never half-apply" applies to writes as much as to
   reloads.
3. **Gate rows** in `ipc/gate.rs`, per file and per direction.
4. **One declarative schema** (COMP-13 §1.5). The parser validates from it,
   `eclipse-ctl` enumerates from it, and DP-5 *generates* its controls from it.
   Parity between the two front ends is generated rather than maintained; a
   hand-built GUI guarantees drift.

**Gate:** set every `abyss.kdl` key through the API and confirm the file
round-trips with comments and formatting intact; confirm that every attempt to
write a `policy.kdl` key from an ordinary client is refused; confirm hot-reload
picks up each successful change.

## DP-3 — Mode profiles

A `mode wm|de` key selecting a bundle of defaults: which autostart set runs,
which keybinds are default, whether the panel and desktop layers launch, what
the decoration defaults are.

**Settle one thing explicitly:** `mode` selects *defaults that explicit config
overrides*, it does not force values. A user who sets `rounding 8` in DE mode
and then switches to WM mode keeps their rounding. Anything else makes the
toggle destructive and people will not touch it twice.

**Gate:** flip the key, hot-reload, watch the session change shape without a
restart.

## DP-4 — DE furniture, on Quickshell *(distribution work — OS-2)*

Taskbar with a real window list, tray, clock, launcher, notifications. Built by
extending the existing `eclipse` Quickshell config; native Rust crates are a
later question, deliberately deferred until the shape has settled.

**This is D-05 default-userland work, not compositor work.** It is tracked as
OS-2 in `claude/OS_WORK.md`, which carries the implementation notes. It appears
here because a taskbar is most of what "DE mode" means to a user.

It is also **dual-purpose**: a taskbar consuming `ext_foreign_toplevel_list` is
the third-party bar that milestone 9's gate has been waiting on, so it closes
M9 as a side effect.

## DP-5 — Settings GUI (untrusted)

A Quickshell client over DP-2's write API, scoped to **`abyss.kdl` only**:
appearance, input, outputs, layouts, keybinds, and the screen-edges selector
from DP-1 — drag the edges, preview live, persist per output. **The overscan
backend is already built and reachable over IPC** (`calibrate_output` with
`start`/`commit`/`cancel`), so this panel is a front end over a working
mechanism, not new plumbing. It also needs the first-boot step that offers
calibration without forcing it.

Because it can only write `abyss.kdl`, it is an ordinary Wayland client. It is
**not** in the TCB, it needs no anti-spoofing story, and it can be sandboxed
freely. That is the whole payoff of the file split: the app users interact with
constantly is the app with no security responsibility.

**Its controls are generated from the config schema** (COMP-13 §1.5), not
hand-built per key. Bespoke controls are allowed where a generated widget would
be poor — the screen-edges selector is the motivating case — but a key with no
control at all is a tracked exception on a list that may only shrink.

**It keeps no persistent state of its own.** Anything it remembers between
sessions lives in `abyss.kdl` and is therefore equally reachable from a shell.
A GUI with its own store breaks CHARTER §4's source-of-truth rule and creates
exactly the GUI-only lock-in COMP-13 §1.5 exists to prevent.

If a user opens the settings GUI and navigates to something policy-owned, the
correct behavior is to show the current value read-only (if reads are permitted
at all) and offer to launch the policy editor below — not to fail silently or
hide that the setting exists.

## DP-6 — Desktop icons *(distribution work — D-05)*

Background layer-shell surface, `.desktop` parsing, icon-theme lookup, drag and
drop, file operations. The largest single chunk here, the lowest security risk,
and the easiest to defer. Explicitly last.

Like DP-4, this is default-userland work rather than compositor work, with the
exception of whatever background-layer support abyss has to provide.

---

## The policy editor is milestone 15, not DP-7

The separate app for agent config and policy is the right idea, and it should
**not** be built as part of this series.

**It is the same trust class as work that is already specified.** COMP-10 §3.9
now names it explicitly as a trusted-UI surface, alongside the consent prompt,
the emergency panel, and the trusted phrase. A policy editor has identical
requirements — the compositor must authenticate it, and the user must be able
to tell it apart from a client impersonating it. Building a second, parallel
trust mechanism for a settings app would be duplicated work and a second thing
to get wrong.

Two pieces of it already exist in the tree, inert: `wp_security_context` (from
milestone 9a) is the attestation primitive, and `agent-attention` (SUPER+space)
is a built-in bind, reserved against rebinding, whose handler only logs
(stub 9). Summon-by-chord is the specified invocation model — a chord the
compositor owns and no client can intercept — and it is half-built.

**There is also nothing to edit yet.** `policyd` does not exist. COMP-11's
enforcement table does not exist. Grants, tasks, leases and the audit spine do
not exist. A policy editor shipped today would edit a capture allowlist and
four window-rule actions. The moment it becomes worth building is when
milestones 10, 11 and 16 have given it a subject.

**What is worth doing now is the file split, not the app.** `policy.kdl` should
exist before more security-relevant keys accrue in `abyss.kdl`, because every
key added before the split is a key that has to be migrated after it. The split
is cheap today and gets more expensive every week. The editor can wait; the
schema decision cannot.

---

## Dependencies and suggested order

DP-3 and DP-4 are independent of everything else. DP-2 blocks DP-5, which now
also carries DP-1's remaining UI half. DP-6 is last. The policy editor depends
on milestones 10, 11, 15 and 16.

1. **DP-4 first** *(OS-2)* — you need a bar to daily-drive at all, and it
   closes M9.
2. ~~DP-1's probe~~ — done, and the backend shipped with it. DP-1's remaining
   half is the calibration UI, which is part of DP-5.
3. **DP-2's file split next, ahead of the write API.** It is a schema decision,
   it is cheap now, and it is the one item here that gets more expensive the
   longer it waits.
4. **DP-3 anytime** — small.
5. **DP-2's write API, then DP-5, during the fourteen days.** The KDL
   round-trip spike happens before DP-5 is scheduled at all.
6. **DP-6 after.**
7. **Policy editor** — with milestone 15.

## One caution

This is a substantial amount of new scope on a project where Phase 2 — the
agent protocol, `policyd`, `agentd`, the audit spine, the whole reason
EclipseOS exists — still has zero lines of code. Taskbars and settings panels
are table stakes that every desktop already has; the agent stack is the part
nobody else is building. DP-1–DP-6 are worth doing, but they should not be
allowed to become the project. Worth setting a deliberate boundary on how much
of the fourteen days they consume.
