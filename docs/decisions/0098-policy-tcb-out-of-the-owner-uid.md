# 0098 — The policy TCB leaves the owner's uid
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)
Supersedes (on acceptance): 0075 in part (its "same footing as policyd" premise), D-01 §3.4 and §5.4

## Context
The threat model calls applications untrusted (P6) and agents untrusted (P5).
Agents are sandboxed. Applications are not: every ordinary program runs as the
owner's uid with the owner's home. Everything the agent security model rests
on is reachable from there:

- `~/.local/state/eclipse/policyd/issuer.key` is a 0600 file. Any same-uid
  process can read it and sign any grant or policy table. The compositor
  verifies those signatures and cannot tell the difference.
- `ec-agent.sock` admits any peer whose `SO_PEERCRED` uid is the session uid
  (`protocols/agent/mod.rs::peer_is_us`). With a self-signed grant, that peer
  holds an agent seat: input injection, scene reads, capture.
- `~/.config/eclipse/policy.kdl` is owner-writable. COMP-13 §1.3 closes it to
  the control socket; nothing closes it to `echo >>`.
- The audit journal is in the same directory and can be truncated.

So the model holds against exactly one adversary, the sandboxed agent, and
fails against a compromised browser, an npm postinstall script, or anything an
agent launches outside its sandbox (Threat Model §7 allows that by rule). The
specs treat the gap one site at a time: S-01 §6 puts same-uid code outside the
boundary, COMP-10 §3.10 and ADR 0061 add an exe check and a Yama assumption,
D-05 adds the commit slot against a same-uid "laundering hole", KNOWNBUGS
TASK-01 and BROKER-01 record exe-path peer checks. They are patches around a
principal the model never names.

Two facts constrain the fix:

1. **A user-unit cgroup is not an identity.** The owner's uid can add a
   drop-in under `~/.config/systemd/user/` and run any binary as
   `ec-policyd.service`. COMP-13 §3 and F-05's "peer is in the unit's cgroup"
   check therefore proves nothing against same-uid code. Only a *system* unit,
   or a kernel credential the owner's uid cannot assume, does.
2. **The compositor must stay in the owner's logind session** (TASK-01), so it
   cannot become a system unit, and it runs as the owner's uid.

## Options
1. **Declare same-uid code trusted.** Amend the threat model; delete the
   piecemeal defences. Honest and free, but then P6 is not untrusted and a
   compromised app owns the agent stack. Rejected by the owner 2026-11-10.
2. **Confine applications** (Flatpak-style, `wp_security_context_v1`) and leave
   the TCB where it is. Largest effort; breaks the ordinary Arch app model.
   Rejected by the owner 2026-11-10.
3. **Move the TCB's state and identity to credentials the owner's uid does not
   hold.** Chosen by the owner 2026-11-10; the shape is below.

## Decision (proposed shape — each numbered item is owner-reviewable)

### 1. `policyd` is a system service under a static user
`ec-policyd.service` becomes a system unit, `User=eclipse-policy`, with
`StateDirectory=eclipse/policyd` and `RuntimeDirectory=eclipse`. State moves to
`/var/lib/eclipse/policyd/<uid>/` (issuer key, journal, tasks), one directory
per owner uid, so D-01 §3.4's objection ("one journal and one key for all
users") does not arise: the peer's uid selects the directory. The socket is
`/run/eclipse/policyd.sock`. A static user, not `DynamicUser=`, because the
socket's group ownership (item 2) must be stable across restarts.

The issuer key is regenerated on migration. Old grants die with it; they are
short-lived by S-01 §4 and nothing durable is signed by the old key except the
audit chain, which is closed with a final record and kept read-only.

### 2. Kernel credentials, not paths or unit names, identify TCB peers
Two system groups whose only members are binaries, never people:

| Group | Held by | Proves |
|---|---|---|
| `eclipse-tcb` | `/usr/bin/ec-abyss`, setgid | "this peer is the installed compositor" |
| `eclipse-agentd` | `/usr/bin/ec-agentd`, setgid | "this peer is the installed agent gateway" |

`policyd.sock` is `root:eclipse-tcb 0660` plus a second listener (or a role
check on `SO_PEERCRED` gid) for `eclipse-agentd`. `ec-agent.sock` stays in
`$XDG_RUNTIME_DIR` but `accept` additionally requires the peer's gid to be
`eclipse-agentd`. Exe-path peer checks (`ec-policyd/src/peer.rs`) and the
user-unit cgroup check go away.

A setgid binary gets three properties from the kernel at exec, which is the
point of choosing it over a path check:
- `AT_SECURE`: the loader ignores `LD_PRELOAD` and `LD_LIBRARY_PATH`.
- Non-dumpable: a same-uid process cannot ptrace it or open `/proc/<pid>/mem`.
  This turns ADR 0028's and ADR 0061's "stated limit" into a closed case and
  removes the Yama assumption.
- An effective gid the owner's uid cannot acquire any other way.

**The cost is real and is the main review item.** The compositor spawns
programs (binds, autostart, XWayland, the locker, app launches). Every child
must drop the gid before exec or the group leaks to user code. Rule: one spawn
function in `ec-abyss` that calls `setresgid(real, real, real)` in `pre_exec`,
a clippy `disallowed_methods` entry for `Command::spawn` outside it, and a test
that a spawned child reports the owner's primary gid. `ec-agentd` gets the
same rule for agent processes.

**Unverified, needs a spike before acceptance:** that Mesa, the NVIDIA EGL
stack, libinput and XWayland start correctly under a setgid compositor
(`secure_getenv` changes which environment variables they honour). If the
spike fails, the fallback is a 200-line setgid helper that proves identity
once and passes the connected `policyd` fd to an unprivileged compositor; that
keeps the group identity but loses the ptrace and preload protection.

### 3. Policy is not an owner-writable file
User policy moves to `/var/lib/eclipse/policyd/<uid>/policy.kdl`, owned by
`eclipse-policy`, world-readable so `ec-policy-viewer` and `cat` keep working.
`~/.config/eclipse/policy.kdl` is read once at migration and then ignored,
with a logged notice.

Edits go through `policyd`, which applies one only after a compositor-drawn
confirmation that shows the change (Threat Model T7 already requires this; the
COMP-10 §3.9 editor is the eventual surface). Until that editor exists:
`ec-ctl policy edit` opens `$EDITOR` on a copy, validates it, and submits the
result; the compositor draws the confirmation with the diff summary, answered
by the human seat only. This keeps the charter's "hand-editable" and "parity
between terminal and GUI" principles: the file is still text and still edited
in an editor; what changes is that saving it is an authority-widening act and
happens in trusted UI, like every other one (D-05 §3).

The capture/clipboard/global allowlists (ADR 0022, 0027, 0029, 0030, 0081,
0094) live in that file and so inherit the protection without a change of
their own.

### 4. Audit
The journal lives beside the key, writable only by `eclipse-policy`. The owner
reads it through a read-only `policyd` method gated on the peer uid matching
the directory; `ec-audit` uses that. S-04 §4 anchors go to
`/var/lib/eclipse/anchors/`, root-owned, as S-04 already proposes.

### 5. What this does not do
- It does not confine applications. A same-uid program can still read the
  owner's files, drive the control socket (ADR 0028: window titles, focus,
  config writes to `abyss.kdl`) and bind ordinary Wayland globals.
- It does not protect `abyss.kdl`. An exec bind planted there runs as the
  owner, with the owner's authority and nothing more.
- A root compromise is still total.
- `brokerd` stays a user service (ADR 0075): its files are ciphertext under a
  sealed key, which is a different argument. BROKER-01's exe-path peer check
  should move to the same gid scheme in a later change.

After this change the claim the system can make is: *code running as the
owner cannot mint a grant, hold an agent seat, change policy, or alter the
audit record without a compositor-drawn confirmation answered at the
keyboard.* That is the claim the documents already make.

## Milestones (each its own TCB PR, owner line-by-line review)
1. Spike: setgid compositor on Intel and NVIDIA, real KMS and headless.
2. `sysusers.d` entries, system unit, state and socket move, per-uid
   directories, key regeneration, migration of the existing journal.
3. Gid-based peer admission in `policyd`, `abyss` (`policy/link.rs`) and
   `ec-agent.sock`; the single spawn function and its lint.
4. Policy file move, `policyd` edit method, confirmation prompt,
   `ec-ctl policy edit`; `ec-settings`/`ec-policy-viewer` follow (frontend).
5. Audit read method and `ec-audit`.
6. Dev workflow: `install-session.sh` and the `ec-update` path gain a
   `chgrp`/`chmod g+s` step (needs sudo once per rebuilt TCB binary);
   `dev-peers` is removed.

## Must be amended on acceptance
D-01 §3.2, §3.4, §4.2, §4.3, §5.4. S-01 §6 and COMP-01 §6 (F-05: session-uid
rule, per-user key). COMP-13 §3 (socket path, peer rule). COMP-01 §5 step 10
(`ec-agent.sock` admission). Threat Model §2 (name the principal: "P6a,
unconfined same-uid code"), T7 ("owned by the human's identity"). S-02 §2.2
(which also disagrees with D-01 §4.2 and ADR 0037 about `policy.kdl` versus a
`policy/` directory — settle that in the same amendment). KNOWNBUGS TASK-01,
SEC-07 (#269). STATUS "per-user policyd" entry. ADR 0028 and 0061 stated limits.

## Open, for the owner
1. Setgid compositor versus the fd-passing helper, once the spike reports.
2. Whether the greeter session (ADR 0087) runs with `policyd` at all.
3. Whether `eclipseos-agents` staying an add-on (ADR 0069) means the system
   user and unit ship only with it, leaving a floor install with no policy
   daemon and the allowlists back in an owner-writable file. Proposed: the
   system user and the policy file move ship in the floor; the agent parts
   stay in the add-on.

## Revisit when
Z-03 (multi-user) is taken up, or applications become confinable by default.
