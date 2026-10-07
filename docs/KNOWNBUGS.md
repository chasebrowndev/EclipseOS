# Known bugs

Bugs that are understood, reproducible, and not yet fixed. A bug leaves this
file when the fix lands, not when a cause is identified — if it is diagnosed
but unfixed it stays here with the diagnosis attached.

Each entry carries the file and line where the fault lives, how to reproduce
it, and the proposed fix. "Proposed" means exactly that: nobody has committed
to it yet, and an entry with no proposal is more honest than one with a guess.

Found 2026-09-11 by `eclipse-ui-prober` against the recomposed launcher.
LAUNCH-01 through LAUNCH-04 were fixed on 2026-09-13 and removed from this file.
RAISE-01 was fixed on 2026-09-18 and removed.
BLUR-01, LAUNCH-05, and TERM-01 were fixed on 2026-09-22 and removed.
HW-01 through HW-07 (the first Framework install, 2026-09-19) and PKG-01/PKG-02
(updating a live install, 2026-09-22), PKG-03, PKG-04 and CFG-01 were fixed and removed on 2026-09-23; the
rules they left behind are kept below. The full write-ups are in git history
(this file at `20d8e3a`) and `docs/internal/handoff/2026-09-19-greeter-to-abyss.md`.
BLUR-02 and TILE-01 were fixed on 2026-09-23 and removed. BLUR-02 came back and
was fixed for real on 2026-09-28: the line was ec-toasts' idle 1 px
transparent layer, which the compositor blurred; the stack now has no surface
while empty.
TRAY-01 was fixed on 2026-10-06 and removed: the Settings pane remembers
the tray ids it has listed this session.
BLUR-01's fix (opaque sheets, because clients could not tell whether blur was on)
is superseded on 2026-09-29 (C-14): panes read `decoration.blur.mode` and only tint.

---

# First real-hardware install — Framework 13, 2026-09-19

Every bug found taking the 2026.09.19 ISO to a working session on the owner's
Framework 13 (HW-01..HW-07) is fixed. What they leave behind:

- **A shipped bind must name a binary in `eclipseos-meta`'s dependency
  closure** (was HW-04). `default_binds()` spawns `foot` (Super+Q,
  Super+Return), `ec-launcher` (Super+E, Super+R) and `ec-center`
  (Super+N). `default_bind_spawns_name_shipped_binaries` enforces it against
  `packaging/pkg/eclipseos/PKGBUILD`.
- **User units are enabled by `abyss-session.target.wants/` symlinks the
  packages ship** (was HW-02), not by a user-preset, which only takes effect
  on `systemctl --user preset-all`.
- **A dependency only the dev box has is not a dependency the image has**
  (was HW-01, `xorg-xwayland`).

## Still open from this install

- **`eclipseos-postinstall.sh` on an archinstall btrfs layout is untested.**
  It used to mount the root partition plainly, landing in the top-level
  subvolume. It now mounts the subvolume holding `/etc` (`@` in archinstall's
  layout), then everything the installed `/etc/fstab` names, under `/mnt`
  (2026-10-06). Nobody has walked a btrfs install through it yet; only ext4
  has been. This entry leaves when one has.
- **Remote access is opt-in at install time.** Both installers now ask
  whether to enable sshd, taking one public key and refusing password logins
  (2026-10-06). `tailscale` is still in no EclipseOS package set; it was what
  made the Framework install debuggable, but sshd on the LAN covers the same
  need without a third-party account.

---

# Screen capture — found building blur modes, 2026-09-27

## CAP-01: screenshots and screencasts drop every compositor effect

**Fix landed 2026-10-06, not yet seen on screen.** `capture_elements`
(`abyss/crates/ec-abyss/src/render/capture.rs`, TCB) still takes every
redaction decision itself, then hands each surviving window and layer to
`abyss/crates/ec-abyss-render/src/still.rs`, which adds opacity, dim-inactive,
rounding, shadow, glow and blurred backdrops from its own store. A backdrop
samples only the redacted capture list, placeholders included, so a blurred
secret is a blurred placeholder. Borders and trusted UI stay out, as before.
`still.rs` never reads animation state (`scan_tests.rs` pins it), so a capture
shows the settled frame. There is no GL pixel test; this entry leaves after a
nested-winit check: `decoration.blur.mode "glass"`, a translucent window over a
`capture.redact-app-id` window, `grim` the output, and see rounded corners and
a backdrop with only placeholder grey under the redacted rect.

**Cursor (fixed 2026-09-29):** `wlr_screencopy`'s `overlay_cursor` request
flag was silently discarded, so no client — with or without the flag set —
ever saw the pointer in a capture (COMP-02 §8 requires cursor-in-capture to be
opt-in, not ambiently absent). `copy_one` now bakes the cursor in via
`render::cursor::elements` when the client asked and no trusted prompt holds
the seat. `ext_image_copy_capture_v1`'s separate pointer-cursor session
(`image_copy_capture.rs`) remains a stub — see `PROPOSEDFEATURES.md` Phase 1
M8 — since it needs a metadata cursor stream, not baked-in pixels.

## BLUR-03: a blurred rectangle frames the toast stack

Found 2026-09-28 fixing BLUR-02. While cards show, the compositor blurs the
whole ec-toasts layer (404 x stack height), including the transparent
gaps around and between the rounded cards, so a square blurred slab shows
behind them. Layers are blurred wherever they leave the surface uncovered
(`abyss/crates/ec-abyss-render/src/lib.rs`, the layer pass of `collect_elements`); the toasts surface is larger than what it draws.
**Repro:** translucent window top-right, `notify-send` twice; see the slab
around both cards. **Compositor half fixed 2026-10-06:** abyss implements
`ext_background_effect_v1`
(`abyss/crates/ec-abyss/src/protocols/standard/background_effect.rs`), and a
committed blur region replaces the whole-surface backdrop, live and in
captures. **Still open:** ec-toasts must set a region per card. That is
frontend (`eclipse-frontend`), and the entry leaves when it lands.
**Blocked 2026-10-06 on iced_layershell 0.19.1:** the connection is reachable
(`Settings::with_connection` takes a shared `wayland_client::Connection`, so a
second event queue can bind `ext_background_effect_manager_v1`), but the
layer surface's `wl_surface` is not. `window::run` / raw-handle actions fall
through (`WindowAction::_ => {}` in `multi_window.rs`), and the only callback
into the window, `SetInputRegion`, hands out a `WlRegion`. Smallest fix: a
`LayerShellCustomAction` that runs a `Fn(&WlSurface)` callback (modelled on
`SetInputRegion`), upstream or as a `[patch]`; the rest is client-side only.

---

## Probing notes for the launcher

The Hyprland-host probing recipe (`hyprctl`, `ydotool` scale) that used to sit
here is retired with the Hyprland dev host. One note still holds: the journal
is silent under both `-t ec-launcher` and `-t abyss` for the
whole of a launcher probe. That is correct, not a fault: the launcher holds no
capability, and logging the query would violate the never-log-human-input
invariant. Visual state is the only oracle here, so silence is never evidence
of a dead control in this crate.

---

# Agent seats — found landing M13, 2026-10-06

## SEAT-01: an agent's wl_seat is advertised to every client

`abyss/crates/ec-abyss/src/protocols/agent/seat.rs` creates each agent seat
with smithay's `SeatState::new_wl_seat`, whose global has no per-client
filter. Every client on the main display can bind `agent-<id>`, see its
capabilities, and bind text-input or input-method objects on it. Input is
still only *delivered* by the agent through `policy::enforce`; what leaks is
that the seat exists and its focus events. **Repro:** start an agent, call
`get_seat`, run `wayland-info` on the main socket; `agent-1` is listed.
**Proposed:** none committed. Either a filtered seat global hand-written like
`output_power.rs`, or bind visibility to clients the agent has focused.

## SEAT-02: xdg_activation from agent-launched clients is not checked

COMP-04 §7 says an agent-launched client cannot steal human focus through
`xdg_activation`. `protocols/standard/xdg_activation.rs` has no notion of
which client an agent launched (agents cannot launch yet), so the rule is not
enforced. It becomes live with agent `launch` (COMP-08); fix it there.


## TASK-01: policyd authenticates its peers by executable, not by unit

policyd now creates a task only from `create_task` on a preview it issued
(`ec-policyd/src/dispatch.rs`, A-08 §5.2); the compositor's commit slot that
sends it is the console wave's T1 (`docs/internal/console-plan.md`).

`policyd.sock` used to admit any session-uid peer. It now gives each
connection a role from the peer's executable (`ec-policyd/src/peer.rs`):
`/usr/bin/ec-abyss`, `ec-agentd` or `ec-brokerd`, and refuses everything
else and everything under `agents.slice`. Two things remain:

- It is an exe check, not the F-05 unit check, because the compositor must
  stay in its logind session scope and so cannot be a unit. A same-uid
  process that can `ptrace` abyss defeats it; that is outside what any
  socket check can stop (Yama `ptrace_scope` ≥ 1 is assumed).
- Dev sessions run binaries from `target/`, which only the `dev-peers`
  feature accepts. Build `ec-policyd --features dev-peers` for `cargo run`;
  the package build never enables it. `packaging/install-session.sh --agents`
  symlinks the session into `target/release` too, so it refuses a policyd or
  brokerd whose `--build-info` is not `dev-peers`.

## BROKER-01: brokerd gaps against S-08

`abyss/crates/ec-brokerd/` lands milestone 19 with these known gaps.

- **No TPM sealing.** Only the software/mock sealer and the Argon2id
  passphrase fallback exist; the daemon uses the passphrase one. S-08 §9.1
  (policy-signed PCR sealing) is open.
- **Peer identity is an exe-path check**, not the F-05 cgroup check, for the
  reason TASK-01 gives for policyd. The proxy's binary name is a guess until
  M20 lands.
- **Transient plaintext outside locked memory.** The CBOR response buffer
  that carries a value to its consumer, the receive buffer for an `add`, and
  the cipher key schedule on the stack are zeroed after use but are not
  `mlock`ed. `RLIMIT_MEMLOCK` must also cover the store (unit sets it).
- **`bound_to` matching choices the spec does not make:** a binding with no
  port means 443 only; `app:<id>` is a third binding form for `field_fill`
  into native apps; `materialize` does not consult `bound_to` (a sandbox env
  var has no host).
- **No `ec-secret` CLI, no swap scan.** Entering secrets today is the wire
  `add` request; the S-08 §8 swap scan on a synthetic run is not automated.


# Agent batches, generations and locks — found landing M14, 2026-10-06

## BATCH-01: a batch is all-or-nothing only up to what the compositor can check without running a step

`protocols/agent/atomic.rs` queues a batch's steps and checks the whole batch
before running any (paused, a target gone or unmapped, a focus-dependent step
with no focus, a stale generation). `policy::enforce` (TCB) has no "decide
without executing" entry, so a step the *policy* refuses (deny, a prompt that
parks, a rule change mid-batch) is only seen when its turn comes. Earlier steps
have then run: the commit answers that step's status with detail
`partial:<n>`, and a step that parks behind a prompt ends the batch with
`batch_interrupted`. **Proposed:** `policy::enforce::dry_run` (steps 1 to 8 for
every request, nothing executed), called from `atomic::commit` at the marked
`TCB-HOOK`. Also: `max_frames` is validated and carried, but delivery is
synchronous (0 frames), and an open batch is abandoned after 5 s, a constant
the spec does not give.

## GEN-01: generations track what a toplevel reports, not its tree

COMP-09's semantic tree does not exist, so a toplevel's generation bumps on a
change of app id, title, geometry, workspace, output or the
maximized/fullscreen/floating/minimized bits (`generation.rs`). A change
inside the client's content that moves none of those is not seen. Step 8 of
COMP-08 §10 runs in `seat::execute` (after the table, and after the 8b class
re-check for a prompted request) rather than between 7 and 8b, and dedupe
(step 2) runs in `seat.rs` before the request reaches `enforce`. Both want to
move into `policy::enforce` (see the `TCB-HOOK` in `seat::execute`).

## LOCK-01: compat_lock holds human input, but does not yet route agent acts through the human seat

`protocols/agent/lock.rs` queues the human's keys, buttons and scrolls for the
locked window and replays them on release. Not done: the COMP-10 §3.4
indicator (TCB), the table check for `seat.compat_lock` (the grant must hold
it and `seat.focus` reaching the window instead), queueing of touch and tablet
input, and the compat fallback that sends an agent's acts through the human
seat for a `seat-compat lock` app (COMP-04 §8). Queued input is discarded, not
delivered, if the human's focus has left the window by release.

## WAIT-01: wait_for has no semantic or terminal predicates

`node`, `text_contains`, `node_gone` (COMP-09) and `command_finished` are
refused `invalid_argument` "unsupported". A wait is polled every 25 ms, not
woken by the change; `waited` is not audited beyond the request's own record.

---

# Semantic protocol — found landing M22, 2026-10-06

## SEM-01: the semantic tree is stored but nothing consumes it yet

`protocols/semantic/` serves `eclipse_semantic_v1` and exposes
`capture_facts`, `read_tree`, `request_action`/`take_done` and `focus_hint`,
but the three consumers are TCB or agent-protocol code that has not been
switched over: `render/capture.rs::semantics_for` still returns `Absent`, the
`eclipse_agent_v1` XML has no `get_tree`, and `eclipse_agent_seat_v1.action`
does not forward to a publisher. Until they do, node-level redaction is not
live and agents cannot read or act on a native tree. **Proposed:** the
TCB-HOOKs in `protocols/semantic/mod.rs`, in that order.

## SEM-02: gaps against COMP-09 §3

- **Commit coalescing** ("at most one visible generation per frame") is not
  done: every `commit` that changes something is published at once.
- **Popup rectangles** are clamped to the toplevel's `bbox()`, not validated
  against the popup's actual geometry, so a menu extending past the window
  has its node rectangle pulled inside it.
- **`registryd` export** of native trees does not exist.
- **`ext.irreversible`** is dropped always: the S-06 taxonomy is not in the
  table until M18.
- **A tree that vanishes holding a secret** (publisher destroyed, replaced)
  keeps its surface covered whole until the surface dies. This is the
  ratchet, and it will cover a legitimate restart of a login window's
  publisher.
- **Staleness** is a surface resize since the last structural commit, only
  while a secret is known; COMP-02 A-10's "surface's current generation" has
  no other definition in the tree today.


## QUEUE-01: the decision queue is the prompt sequence, not a list

COMP-10 §3.13 (Appendix F-12) describes a list of every parked prompt, each
expanding to its full prompt. Consent prompts in v1 have no "decide later",
so the oldest parked prompt is always on screen whenever the seat is free,
and `trusted_ui::queue::open` (Super+Space, `show_decisions`) brings it
forward with Deny focused. A list view needs deferral first.

---

# Protected surfaces — found landing B1 (console wave), 2026-10-06

## PROT-01: COMP-19 plumbing is in, the TCB half is not

The protocol, registry, input origin and Enter/slot-rectangle isolation are
in `protocols/protected/` and `input/`. Nothing yet draws, arms or commits a
slot, excludes a protected surface from capture, or from the agent scene, and
the audit record for refused agent input is not emitted: each is a
`TCB-HOOK` in `protocols/protected/hooks.rs`, or a call to
`protocols::protected::is_protected`, for main to wire. Until then a protected
surface is only input-protected.

## PROT-02: gaps against COMP-19

- **Origin is a tag on the state, not a field on every event.** The entry
  point sets `AbyssState::input_origin` for its duration and the deliveries
  read it; an event no entry point tagged reads as `injected` (refused on a
  protected surface in a release build). The control socket's `type_text` and
  `click_at` and `zwp_virtual_keyboard` do not exist yet, so `scripted` and the
  keyboard half of `virtual` have no producer, and `agent_compat` has none
  either: compat locks hold the *human's* input, they do not route agent acts
  through the human seat (LOCK-01). The tags and the drop are ready for them.
- **A toplevel holding a protected subsurface takes no non-physical keys, and
  an agent cannot focus it.** The keyboard focus is the toplevel, and the
  compositor cannot tell which widget the client routes a key to, so the
  whole toplevel is refused. COMP-19 does not say; this is the safe reading.
- **A slot on a popup or a layer surface has no rectangle** (`slot_rect` is
  `None`): only toplevels and their subsurfaces are located.
- **Touch inside a slot's rectangle is dropped, not a click.** COMP-19 §6
  names the pointer.
- **`both continuation and resumes`** has no protocol error in COMP-19 §2: the
  draft is refused (`state(refused, refused)`) and not stored.
- **The fifth slot of a client** is the protocol error `slot_exists`; §8 says
  only "at most 4".
- **A focus change with no input event behind it** (a window mapping, closing)
  may focus a protected surface; only focus caused by an event of a
  non-physical origin is refused.
- **`ec-abyss-wlcs` does not enable `ec-abyss/wlcs`.** Doing so from the
  workspace would unify the feature into every `cargo test --workspace`, and
  the "injected is refused" test would stop running; a conformance run must
  pass `--features ec-abyss/wlcs` itself.

## AGENTD-01: agentd gaps against A-08 and A-01

- **The sandbox has no network and no egress.** Landlock, seccomp,
  `no_new_privs`, `--cap-drop ALL`, `fs.read` binds and `MemoryMax`/`TasksMax`/
  `CPUQuota` on the scope are in (`ec-agentd/src/sandbox.rs`, `launcher.rs`; VOL1
  §7, A-07 §2, §6); the agent runs with `--unshare-net`. A-07's
  `sandbox { net.egress ... }` needs the S-09 egress proxy (milestone 20), which
  does not exist, so a package declaring `net.egress` is refused at launch,
  reason `egress_unavailable` (`Exited{failed}`). So is an unreadable or unknown
  `sandbox { }` key, an `fs.read` that reaches `~/.ssh` and the like
  (`fs_read_forbidden`), and an `fs.write` outside the scratch
  (`fs_write_outside_scratch`). A kernel without Landlock refuses every launch
  (`sandbox_unavailable`); there is no degraded mode outside `dev-unsandboxed`.
  Not done: the filesystem allow-list is built from the manifest's declarations,
  not from the signed grants (agentd does not decode grants); seccomp is a
  fixed deny-list, with no per-package profile; Landlock does not restrict
  `connect()` to a unix socket path before ABI 9 (the MCP socket is the only one
  bound in, so it does not matter); the slice is `agents-<pkg>.slice`, not
  VOL1 §7's `agents.slice/agent-<id>.slice`.
- **agentd's unit allows `AF_NETLINK`.** Agents launch through
  `systemd-run --scope`, so bwrap runs in `ec-agentd.service`'s process tree
  and inherits its `RestrictAddressFamilies`; bwrap brings the sandbox's loopback
  up over `NETLINK_ROUTE`, so the unit allows `AF_UNIX AF_NETLINK` (a launcher
  test holds the two together). The agent's own seccomp then denies every
  netlink protocol but `NETLINK_ROUTE`. Moving the launch to a transient service
  would keep agentd at `AF_UNIX` but loses the scope's child process the reaper
  waits on. Two VOL1 C-00 seccomp items are not enforced: `bind` on families
  other than `AF_UNIX` (seccomp cannot read the sockaddr; it needs Landlock's
  network rules, ABI 4) and denying `pidfd_getfd` and `personality`.
- **Agent chain is a summary.** An agent post carries `min_trust` from the
  task (always `standard` today) and the agent principal as head. Nothing feeds
  the real S-07 chain in, so `min_trust` never reads `untrusted`.
- **No continuation context.** A-08 §5.3's first `context` message is not
  written: agentd is not given the predecessor's summary or chain.
- **Session records are MCP-level, not model-level.** agentd writes
  `$XDG_STATE_HOME/eclipse/sessions/<task>/record.jsonl` (0700/0600, append-only,
  30-day retention, removed by `delete_session`): every request and response on
  the task's MCP socket plus every conversation message. There is no inference
  path yet (F-21, I-02), so the model's own context, thinking and tool
  arguments it never sent through agentd are not in it. A task resumed with
  `resumes` gets the extra tool `session.restore` (once; the old entries and
  then the boundary marker). The record stops at 32 MiB per task. Not done:
  `list_sessions` marks `eligible` from the manifest's `resumable` (default true
  for `local`, false otherwise, F-24), the install, and a record on disk, but not
  from the close reason (agentd is not told an S-11 I3/I5/I6 closure, A-08
  §5.4); the first `human` message carrying an optional new instruction and
  the slot's statement are policyd's and the console's, not agentd's; a resumed
  task's `min_trust` is copied from the old task, which today is always
  `standard` (the previous bullet), or `untrusted` when the old task is unknown
  to agentd; the console divider for `resumes` is the console's.
- **A task open at restart is closed.** agentd marks it `closed`, reason
  `agentd_restart`, and sends policyd `Exited{failed}` once the link is up
  (queued until then). The agent's scope is not reaped by name or by search.
- **`pending_decisions` per task is 0.** Only the global count reaches agentd
  (`decisions_pending`), and A-08 §7's per-task figure has no source.
- **`ec-ipc` has no `decisions_pending` event kind.** agentd reads that
  stream on its own connection instead of through `ec_ipc::Client`.
- **Quota breach audit body.** A breach is a `channel` record with
  `op: "quota_exceeded"` and `msg_id: 0`; the C2 contract names only
  `post|read`. policyd must accept the third op.
- **SIGTERM stops agents; SIGKILL does not.** A crashed agentd leaves agent
  scopes running until their tasks close.


## SLOT-01: commit slot gaps against COMP-19

- **Occlusion is by windows only.** A layer surface above the host (a
  panel, a notification) does not count as occluding the slot. The card is
  not drawn while a window overlaps it, rather than drawn dimmed (§7), because
  the trusted pass composites above every client.
- **Whole-window exclusion.** A window holding any protected surface is
  left out of capture and out of every agent's scene as a whole, not just
  its protected subtree (`render/capture.rs`, `policy/scene.rs`). Stricter
  than §4 and §1; the console's panes are all protected anyway.
- **`slot_arm_ms` is the default.** The compiled table carries no value for
  it, so every slot arms after 500 ms.
- **Resume restores nothing yet.** policyd and the slot accept `resumes`
  (A-08 §5.4); agentd's session record and `session.restore()` are the next
  commit.

## CONSOLE-01: console wave client pieces are unproven against live peers

- `ec-console-client::protected` has never run against abyss: its protocol XML
  is a client-only copy written from COMP-19 §2, and the `state` event's
  numeric order (previewing=0 .. refused=4) is assumed from the spec's list.
  Compare with `ec-protocols`' canonical file when B1 lands.
- `ec-console-client::console` parses replies tolerantly from assumed JSON
  shapes (see the B3 hand-off); agentd's real server (B2) is the authority.
- `ec-ref-agent` expects `task.inbox` to return `{messages, closed}` as the
  bare result, `structuredContent`, or JSON text in `content[0].text`.
- `ipc::hooks` stubs fail closed (`agent_install` refuses, the queue never
  opens, the pending count is 0) until main wires the TCB side.
