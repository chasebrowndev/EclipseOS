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


## TASK-01: no production path opens a task, and policyd cannot tell abyss from a script

A-08 §5.2 makes the COMP-19 commit slot the only way a task is created:
abyss previews, the human presses Enter on the slot, abyss sends
`create_task` to `policyd`. The slot does not exist yet, so nothing in a
running session opens a task or issues a first grant; only tests do. The link
message (`open_task`) and `TaskStore::open_for_human` are in place for it.

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
  the package build never enables it.

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

