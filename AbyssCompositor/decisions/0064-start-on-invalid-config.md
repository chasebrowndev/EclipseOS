# 0064 — An invalid config at startup drops the bad nodes and starts
Status: accepted
Date: 2026-09-25
Deciders: chase (owner), Claude (advisory)

## Context
COMP-01 §5 step 3 and COMP-13 §1.2 say an invalid config at startup is a
refusal to start. `crates/abyss/src/main.rs` does exactly that: it prints every
`ConfigError` to stderr and exits 1 with "refusing to start on an invalid
config".

The owner hit the cost of that rule. One key the running build did not know
(`touchpad { click-method … }`) made Abyss exit on every login. greetd bounced
each attempt straight back to the greeter, and the reason was only in the
journal. The owner could not reach it without a second machine or a TTY. A
refusal that the user cannot see does not protect anyone. It just locks them
out.

The rule was written against a real risk: a typo that silently does nothing.
That risk is about silence, not about starting. Unknown keys can stay errors
and still be shown to the user, without taking the session down.

Two earlier decisions touch this:
- **ADR 0016** says an invalid config never takes down a running session, and
  the last good config stays live. That covers hot reload and is unaffected.
  At startup there is no last good config, so 0016 does not decide this case.
- **ADR 0033** says an explicit `render-device` that does not resolve refuses
  to start, because falling back to auto hides the mistake on a multi-GPU box.
  A dropped `render-device` node would fall back to auto in exactly that way.

`Config::load` already records a refusal per node (`Config::reject`) and keeps
applying the nodes after it. It also skips a whole file that does not parse as
KDL. The only thing that turns this into a fatal error is the check in
`main.rs`.

## Options
1. **Keep refusing to start.** Simple and total, but any key from a newer
   build, a stale `abyss.d/` snippet, or a typo locks the user out of the
   graphical session. Under greetd the reason is invisible.
2. **Downgrade unknown keys to warnings.** Abyss starts, but a typo that
   silently does nothing comes back on hot reload and on the write API too.
   This contradicts FOG and COMP invariants that validation is total.
3. **Start with the rejected nodes dropped, show the errors, and keep a short
   fail-closed list.** Abyss comes up in every case where a default is safe.
   The error is still an error: it is reported, and hot reload and writes
   still refuse. The cost is a startup that half-applies a config, which
   COMP-13 §1.2 forbade.

## Decision
Option 3.

**At startup** each rejected node is dropped. The setting it would have set
keeps its built-in default, or the value from a lower-precedence file on the
COMP-13 §1 search path. Every other valid node applies. A file that does not
parse as KDL contributes nothing, and an unreadable `--config` file is treated
the same way. Abyss starts.

**The errors are reported twice.** Each one goes to stderr and the journal
with file, line, column and the offending token, as today. Each one is also
shown in the session on the same surface a failed hot reload uses (COMP-13
§1.2: trusted UI and the `config-error` IPC event). No client is connected at
startup, so the startup errors are kept and delivered to every subscriber that
connects, until a load succeeds with no errors. `eclipse-ctl config validate`
reports them as well.

**Hot reload is unchanged.** An invalid edit keeps the last good config live
and applies nothing (ADR 0016, COMP-13 §1.2). After a startup that dropped
nodes, the last good config is the config Abyss started with.

**These cases still refuse to start**, because they have no safe default to
drop to:
1. **Any error in `policy.kdl`.** The capture policy and the rest of the
   security surface (COMP-13 §1.3, ADR 0037) stay fail-closed. This ADR does
   not govern that file. Dropping a `windowrule "sensitivity secret"` or
   `"no-agent"` rule would widen what agents and capture clients can see.
2. **A policy-owned key in `abyss.kdl`** (a `sensitivity`, `app-trust`,
   `seat-compat` or `no-agent` window rule, `misc.scripted-input`, `capture`,
   `clipboard`). COMP-13 §1.3 makes a misplaced key an explicit startup error.
   Dropping it would silently discard a protection the owner meant to have.
3. **`misc { render-device … }`**, whether it is rejected at parse time or
   does not resolve. Falling back to auto is the silent fallback ADR 0033
   rejected. The same applies to `--render-device` and
   `ECLIPSE_RENDER_DEVICE`.
4. **A rejected `idle { lock-timeout-seconds … }` or `idle { lock-command … }`.**
   The default is to never lock by itself (COMP-03 §7, `Idle` in
   `config/mod.rs`: with no `lock-command` the timeout is inert). Dropping
   the node would start a session that never locks, even though the owner
   asked for one that does.

**These keep their defaults whatever is dropped:**
- **The human override** (C-00 §4.5, COMP-04 §6): Super+Escape, which cannot
  be disabled. The same holds for `agent-attention` on Super+space (COMP-13
  §1.1). The parser already refuses to bind either chord. Config binds extend
  the default table (`merge_binds`), so a dropped `bind` node cannot remove
  them. If the override chord is ever made configurable, a rejected
  configuration of it falls back to Super+Escape, and never to unbound.
- **The lock binding** (Super+Shift+L → `loginctl lock-session`). This works
  the same way. A dropped node leaves the default bind in place.

## Consequences
- Unknown keys, bad values and misplaced keys are still **errors**, not
  warnings (COMP-13 §1.2, and the FOG and COMP invariants that validation is
  total). They are no longer fatal at startup, except in the four cases above.
  Hot reload, `validate_config` and `set_config_value` still refuse, as
  before.
- A config written for a newer build no longer locks a user out of an older
  one. A bad `abyss.d/` snippet costs only its own nodes.
- A bad `policy.kdl` can still bounce a greetd login. That is deliberate.
  `policy.kdl` is the file where a guessed default is a hole.
- A file that does not parse as KDL has no nodes to sort, so it is dropped
  whole, even if it held one of the fail-closed keys above. This is accepted.
  The error still names the file, and `policy.kdl` is refused whatever breaks
  it.
- Startup now half-applies a config, and COMP-13 §1.2 used to forbid that.
  The spec is amended (COMP-01 §5 step 3, COMP-13 §1.2) to point here.
- Owed: `main.rs` stops exiting on `config.errors`. It still exits when an
  error falls in the fail-closed set. That needs `ConfigError` to carry
  whether the error is fatal at startup, or the owner and key it came from.
  The startup errors need to be kept for replay on `config-error`. Owed tests:
  one per fail-closed case, one showing that a dropped `bind` or `idle` node
  leaves Super+Escape, Super+space and Super+Shift+L bound, and one showing
  that an unknown key starts with the other nodes applied.

## Revisit when
A dropped node causes a problem the owner does not see because the in-session
notice was missed. Or a new key lands in `abyss.kdl` whose default is unsafe:
it joins the fail-closed list, or it moves to `policy.kdl`.
