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
with file, line, column and the offending token, as today. They also go out
as one `config-error` IPC event, the one a failed hot reload emits.
`eclipse-services` turns it into a persistent "Config problem" notification.
This is **not trusted UI**. The notification is drawn by an ordinary client
and uses a fixed id (`u32::MAX`), so any same-uid D-Bus client can close or
replace it. It is a best-effort notice, not a guarantee that the owner sees
the error. Showing it in compositor-drawn trusted UI (COMP-10) is future work,
and it needs owner and TCB review.

No client is connected at startup, so the compositor keeps the **latest**
`config-error` event and replays it on every `subscribe` that asks for it,
for as long as it stands. At first that is the startup set, labelled
"ignored". A failed hot reload replaces it with its own set, labelled "change
not applied". While the live config still has auto-lock or Xwayland off from
the degraded start, that reload summary keeps the "auto-lock is OFF" /
"Xwayland is OFF" lead ahead of "change not applied". The notification has one
fixed id, so a later reload could otherwise downgrade the warning. A clean load
clears it. `eclipse-ctl config validate` reports
the errors as well.

**The summary leads with a protection that did not take.** When a refusal
left auto-lock or Xwayland off (below), the one-line summary starts with it,
for example `abyss.kdl: auto-lock is OFF — line 5: …` or
`abyss.kdl: Xwayland is OFF — line 2: …`. Only then does it give "(and N
more)", so the count cannot hide the notice.

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

**These start, but in a safe state rather than the default:**
- **`xwayland`, fail closed to off.** `xwayland.enable` defaults to `true`,
  and ADR 0026 treats "off" as the strongest isolation. A dropped node could
  bring the X server up for an owner who turned it off. So if any refusal
  touches `xwayland`, Abyss starts with **Xwayland off**: the safe value, not
  the default. That covers a bad value (a non-bool `enable "false"` is now
  refused instead of silently ignored), an unknown child
  (`xwayland { enabled #false }`), a bad `scaling`, the misplaced
  `misc { xwayland … }`, and a misspelt node (`xwyland { … }`, see the rule
  below). The summary leads with "Xwayland is OFF".
- **`idle` lock settings: start anyway, auto-lock off.** *Owner decision,
  2026-09-25.* An earlier draft of this ADR refused to start on a rejected
  `lock-timeout-seconds` or `lock-command`. The owner chose to start instead.
  Auto-lock stays at the built-in default, which never locks by itself
  (COMP-03 §7; with no `lock-command` the timeout is inert), and the user is
  told plainly. The trade-off: a session the owner meant to auto-lock can run
  unlocked until they read the notice and fix the file. The alternative is a
  greetd login loop over a lock setting. Super+Shift+L still locks by hand.
  Every refusal inside `idle` counts except one about DPMS (`dpms-*`), which
  is not a protection. That includes a misspelt key (`lock-timout-seconds`)
  and a misspelt node (`idel`), which would otherwise leave the session never
  locking without a word. The "auto-lock is OFF" lead is dropped when both
  lock keys still ended up set, for example from a lower-precedence file,
  because then it would not be true.

**The misspelt-node rule.** An unknown top-level node within Levenshtein
distance 2 of `idle` or `xwayland` is taken as an attempt to configure that
protection (`misspelt_guard` in `config/mod.rs`). The rule is deliberately
simple and deterministic. A false hit only makes the notice louder, or, for
`xwayland`, starts without X11, which is the safe direction.

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
  total). They are no longer fatal at startup, except in the three cases above.
  Hot reload, `validate_config` and `set_config_value` still refuse, as
  before.
- After a degraded start, Settings cannot save anything. `set_config_value`
  re-loads the whole search path after its write and rolls the write back if
  that load has any error (`ipc/config_rpc.rs`). The dropped nodes are still
  in the file, so every write is rolled back until the user fixes the file by
  hand. The rollback error names the first refusal, so at least it says why.
- A config written for a newer build no longer locks a user out of an older
  one. A bad `abyss.d/` snippet costs only its own nodes.
- A bad `policy.kdl` can still bounce a greetd login. That is deliberate.
  `policy.kdl` is the file where a guessed default is a hole.
- A file that does not parse as KDL has no nodes to sort, so it is dropped
  whole, even if it held one of the fail-closed keys above. This is accepted.
  A wholly unparseable `abyss.kdl` behaves as if there were no user config:
  Xwayland comes up and auto-lock is off, the same as a fresh install. The
  xwayland and idle fail-safes apply only to a file that parses. The error
  still names the file, and `policy.kdl` is refused whatever breaks it.
- Startup now half-applies a config, and COMP-13 §1.2 used to forbid that.
  The spec is amended (COMP-01 §5 step 3, COMP-13 §1.2, §1.3) to point here.
- Implemented: `Config::startup` (refuse on `ConfigError::startup_fatal`,
  settle `ConfigError::fail_safe`) and `AbyssState::config_error` (replay).
  Tests: `config::startup_tests`, the render-device resolve half in
  `backend::drm`, and `ipc::methods::tests::subscribe_replays_the_current_config_error`.

## Revisit when
A dropped node causes a problem the owner does not see because the
notification was missed or closed by another client. That is the case for
moving the notice into trusted UI. Or a new key lands in `abyss.kdl` whose
default is unsafe: it joins the refuse list or the fail-safe list, or it moves
to `policy.kdl`.
