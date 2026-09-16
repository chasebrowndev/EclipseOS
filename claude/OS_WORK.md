# OS and distribution work — session plan

Split out of `claude/DAILY_DRIVE_PLAN.md` on 2026-09-10 so the compositor plan
stays compositor-only, then rewritten as a standalone session plan.

## The three buckets

EclipseOS work divides into three, and the spec tiers already draw the lines:

| Bucket | What | Specced in | Status |
|---|---|---|---|
| **Compositor** | `abyss`, `eclipse-ctl` | Tier 1 — C-00, COMP-01..17 | Phase 1 substantially built; see `STATUS.md` |
| **System services** | `policyd`, `agentd`, `brokerd`, sandbox, egress proxy | Tiers 2–3 — S-01..S-12, A-01..A-07 | Zero code. Phase 2, milestones 10–21 |
| **Distribution** | base package set, repo, ISO, installer, default userland | Tier 6 — D-01..D-08 | Every document `planned`. Phase 6 |

`claude/DAILY_DRIVE_PLAN.md` covers the first. `STATUS.md`'s Phase 2 table
covers the second. This document covers the third and nothing else.

**System services are not distribution work.** `policyd` and `agentd` are
daemons, so they read as "OS", but they are Tier 2–3 and sequenced as Phase 2
milestones. They are the product; the distro is the wrapper. Do not let them
drift into this file.

---

## The shape of an OS session

**Almost all Tier 6 work is writing documents, not code.** Every one of
D-01..D-08 is `planned` — none exists. The project's ordering rule is that a
document is written only after its dependencies, because downstream specs
inherit decisions, and D-03 (ISO and installer) depends on D-01 existing first.

So there are two quite different sessions available, and they should not be
confused:

- **Session A — small, in-phase, buildable today.** Hours, not days. Unblocks
  the fourteen-day clock. Produces working files, not specs.
- **Session B — long, spec-writing, formally out of phase.** Days. Produces
  D-01 and the scope of D-03. Writes no shippable code at all.

---

## Session A — what the daily-drive clock actually needs

Three items, all Tier 6, all pulled forward into Phase 1 deliberately because
Phase 1's exit gate depends on them. Recognise them as distribution work living
early, not as compositor scope creep.

### OS-1 — Session installation and greeter integration *(D-01, systemd layout)*

The compositor half stays in the daily-drive plan: `abyss --session` performing
the D-Bus and systemd handoff via `session::import()` and `teardown()`, both
best-effort shell-outs today (ADR 0032). Whether that code works is a
compositor question.

The OS half is placement and greeter configuration, and has **never run**:

- `dist/abyss.desktop` → `/usr/share/wayland-sessions/`
- `dist/abyss-session` → `/usr/bin/`
- `dist/abyss-session.target` → the user's systemd units
- greetd configured to offer the session and hand off cleanly

Every real KMS boot so far was launched by hand on a VT with `openvt` as root,
under a `timeout`. Nothing has ever come up from a greeter.

**Before the first attempt:**

- **Keep Hyprland's session entry installed.** If abyss fails at the greeter,
  recovery is picking Hyprland from the same menu.
- **Keep an ssh path in.** `chase-laptop` (`100.124.173.22`) is on the tailnet.
  A greeter that will not hand off is far less alarming with a shell already
  open on the box.
- **Abyss refuses to start on an invalid config** (spec gap 4). Hot-reload
  keeps the last good config, so a typo mid-session is safe, but a typo saved
  before a reboot means no session. Validate before logging out.

Budget: an afternoon, most of it recovering from the first failure.

### OS-2 — Default userland: the bar *(D-05)*

Extend the existing `eclipse` Quickshell config into a taskbar with a real
window list over `ext_foreign_toplevel_list`, plus tray, clock and launcher.

**Dual-purpose:** also the third-party bar that COMP-16 milestone 9's gate has
been waiting on, so it closes M9 as a side effect. And you cannot daily-drive
without a bar. That is why a Tier 6 item runs in Phase 1 (Appendix B, B-08).

Two things not to rediscover:

- `window_closed` fires on **destroy**, not on workspace switch. A consumer
  that conflates them looks broken for reasons that are not abyss's fault.
- `ext_foreign_toplevel_list` hands the full window list to any client that
  binds it. This wants a policy check (COMP-08/COMP-11) that does not exist
  yet. Acceptable for now; record it as a known exposure rather than forgetting
  it.

QML, not Rust.

### OS-3 — Default cursor and icon themes *(D-05)*

Split across buckets, and the split matters:

- **Compositor:** `render/cursor.rs` loads no xcursor theme — every named
  `wp_cursor_shape_v1` shape falls back to a built-in 12x19 amber arrow, a
  speck on a 2x output. Loading the user's theme is abyss code and lives in the
  daily-drive plan.
- **OS:** which theme EclipseOS *ships* as default, and packaging it.

The compositor half blocks daily use. The OS half can wait.

---

## Session B — the long one: write D-01

### Say the out-of-phase part out loud

F-01 §8 says phases are **sequential, not parallel**, because solo capacity
cannot sustain otherwise, and Phase 6 is where distribution lands. The charter
also explicitly removed an early ISO phase:

> *(Former "minimal ISO" phase removed: owner already daily-drives stock Arch,
> so there is no distribution gap to close first. ISO/installer work moves to
> Phase 6.)*

So a long Tier 6 session is a deliberate excursion out of phase order. That is
allowed — it is the owner's charter — but it should be a decision, not a drift.

**The honest argument for doing D-01 early**, independent of enthusiasm:
several D-01 decisions are things abyss code needs *now*, and every week they
stay unmade, code accretes assumptions about them.

- **Where does `policy.kdl` live?** Appendix B B-04 splits config into two
  files with different trust levels, and the whole point is that the policy
  file can be given different ownership or location. That location is a D-01
  filesystem-layout decision, and DP-2 implements against it.
- **What is the systemd unit layout?** `abyss-session.target` exists already;
  `policyd`, `agentd` and `brokerd` will need ordering, sockets and restart
  policy relative to it. Getting this wrong once means migrating units later.
- **What is the user and group model?** This box already exposed the seam:
  `chase` is in `video` and `input` but not in a `seat` group, which is why the
  KMS boot had to run as root with `LIBSEAT_BACKEND=seatd`. A distro that ships
  abyss has to answer this for every user, not work around it per-machine.

None of the rest of Tier 6 has that property. D-02, D-03, D-04, D-06 and D-08
can wait for Phase 6 without costing anything today.

### D-01 agenda

The document to write. Depends on F-01 (done), so it is unblocked.

1. **Base package set.** What is in a minimal EclipseOS install and what is
   merely available. The agent stack is not optional; the desktop userland is
   the interesting boundary now that Appendix B B-01/B-02 commit to GUI
   configurability out of the box.
2. **Kernel.** Stock Arch kernel or a custom build. Whether any patches are
   required. `nvidia-open-dkms` is the reference-hardware driver (F-04), and
   `nvidia-drm.modeset` handling is already a known sharp edge — abyss's guard
   at `drm.rs:141` cannot even read the parameter as a normal user.
3. **Init and systemd layout.** System units versus user units. Ordering and
   socket activation for `abyss-session.target`, and where `policyd`, `agentd`
   and `brokerd` attach when they exist. Restart policy for a TCB daemon.
4. **Filesystem layout.** `/etc/eclipse` versus `~/.config/eclipse`; where
   `abyss.kdl` and `policy.kdl` live and who owns them; where the audit journal
   (milestone 12) will land; where per-agent sandbox images (milestone 20) go.
5. **User and group model.** Seat access, `video`/`input`/`seat` groups, what a
   fresh user needs to run abyss without root, and how that interacts with
   libseat preferring logind.
6. **Shipped defaults.** Which `abyss.kdl` and `policy.kdl` a fresh install
   gets. Note that a shipped `policy.kdl` is a security default, not a
   convenience default, and deserves the same scrutiny as code.

### After D-01, in dependency order

Do not start these before D-01 exists; they inherit its decisions.

| ID | Document | Depends on | Note |
|---|---|---|---|
| D-02 | Package repository: build infra, signing, mirrors | F-07, S-12 | Blocked on S-12 (supply chain), which is also unwritten |
| D-03 | ISO build (archiso) & installer | D-01 | Carries the §7 gate: fresh install to working agent session in under 30 minutes |
| D-04 | Update strategy: rolling vs snapshots, atomic updates, rollback | D-02 | Rollback semantics for a TCB are the hard part |
| D-05 | Default userland: bar, launcher, terminal (`cataclysm`, P-04), portal, notifications | C-00, P-04 | Partly pulled forward as OS-2/OS-3; also blocked on `cataclysm`, which does not exist (defect 12) |
| D-06 | Hardware support matrix, GPU drivers, firmware | F-04 | Reference hardware already decided: NVIDIA-first, Intel iGPU tested, AMD validated before release |
| D-07 | First-run experience & agent onboarding | D-03 | Rises in importance under Appendix B B-02 |
| D-08 | Telemetry & crash reporting (opt-in) | F-02 | — |

### Two §7 gates that constrain Tier 6 long before Phase 6

Worth keeping visible while writing D-01, because they are the acceptance
criteria D-03 and D-07 will be measured against:

- Fresh install from ISO to working agent session in under 30 minutes, with
  documentation only.
- Cold boot to agent-ready under 20 seconds on reference hardware.

And one proposed by Appendix B B-02, not yet written: *a user can reach a
working configuration without editing a file.*

---

## Naming: avoid the D-collision

The desktop-profiles plan originally numbered its items **D1–D6**, colliding
with Tier 6's **D-01–D-08**. That series is now **DP-1–DP-6**
(`claude/DESKTOP_PROFILES_PLAN.md`).

- **D-0n** — Tier 6 distribution documents. This file.
- **DP-n** — desktop profile work items. Mostly compositor code.

`DP-4` (taskbar) is OS-2 above. `DP-6` (desktop icons) is D-05 userland.
