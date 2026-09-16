# 0037 — The security surface moves to its own file, policy.kdl
Status: accepted
Date: 2026-09-10
Deciders: chase (owner), Claude (advisory)

## Context
Appendix B added F-01 §4, "configurable without a text editor": every setting must be
reachable through a GUI, files stay the source of truth, and the GUI writes only through
the COMP-13 §1.4 API. That API will hand out a write capability. A single `abyss.kdl`
holding both `general.gaps-in` and `capture.allow` would force that capability to be
all-or-nothing: either the settings app can rewrite the redaction policy, or it cannot
change the gap size.

COMP-13 §1.3 already calls for file ownership; nothing implemented it. `policy.kdl` did
not exist. `schema.rs` (ADR 0036's sibling, landed in A1) already labels every key with
an `Owner`, so the data needed to split is in the tree.

## Options
1. One file, per-key write gating in the IPC layer. Simplest diff. But "what is the
   policy on this machine" has no answer a human can read, and the file cannot be shipped
   root-owned without also freezing the cosmetic keys.
2. Two files, `abyss.kdl` and `policy.kdl`, ownership enforced at parse time.
3. Two files plus a `policy.d/` drop-in directory, mirroring `abyss.d/`. Rejected: the
   security surface should be one file per directory, so reading it is one `cat`.

## Decision
Option 2. `search_path()` returns `Vec<Source> { path, owner }` and emits each directory's
`policy.kdl` *after* its `abyss.kdl` and after any `abyss.d/` drop-ins, so policy is never
shadowed by a later cosmetic file. `Config.cur` carries the owner, and the parser refuses a
key found in the wrong file — **in both directions**, naming the file it belongs in.

Enforcement sits at three granularities, because ownership is not uniformly per-node:
- whole node, via `schema::node_owner`, checked in `apply()` before the node is parsed;
- per key in `misc`, the one mixed block (`render-device` abyss, `scripted-input` policy);
- per action in `windowrule`, via `schema::rule_owner` (`float` cosmetic, `no-agent` and
  `sensitivity` policy).

An explicit `--config` names an `abyss.kdl` only; `policy.kdl` stays on the search path, so
a command-line flag cannot swap the policy file.

There is no auto-migration. `eclipse-ctl config migrate` will do the local file surgery,
preserving each node's leading trivia, backing up both files, and refusing to commit if
either result fails to re-parse.

## Consequences
- The COMP-13 §1.4 write API can hold Abyss/Write and not Policy/Write. That gate row can
  be asserted unconditionally false rather than made toggleable.
- A deployment can ship `/etc/eclipse/policy.kdl` root-owned and leave the user's
  `abyss.kdl` writable.
- Hot-reload covers `policy.kdl` for free: `watch.rs` derives its watch directories from
  `Config.sources`.
- Existing single-file configs keep working; policy keys in them now error with a message
  naming `policy.kdl`. That is a deliberate hard failure — silently honouring them would
  leave the write gate bypassable.
- We owe: the migrate verb (A5), `docs/CONFIG.md`, and a doc note on the new file.

## Revisit when
A third ownership class appears (a per-app or per-seat surface), or `policy.kdl` grows
past what one screen of `cat` can carry — at which point `policy.d/` is back on the table.
