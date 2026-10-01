# 0067 — Command widgets run only once the owner approves them in Trusted UI
Status: accepted
Date: 2026-09-26
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0065 lets `abyss.kdl` define command widgets (`exec` / `stream`) that
hyperion runs, and adds `set_config_collection`, so any owner-uid socket peer
can add one. A socket peer can therefore cause a command to run, with nothing
between the write and the run. ADR 0065 left this open.

The owner's rulings:
- Widgets EclipseOS ships (**premade**) are hashed. If one is altered, the owner
  is told, per widget: "This widget has been altered! Altered widgets are not
  guaranteed to be safe!", and chooses to accept the alteration or revert to the
  original. Accepting records the new hash, so a further change asks again.
- Widgets that are not premade carry a disclaimer: EclipseOS is not responsible
  for what they do, with a short paragraph on the risk.
- The prompt is drawn by abyss, and exists only when hyperion is installed
  (ADR 0066: the `taskbar-widgets` hook).

Forces:
- A prompt drawn by a client can be imitated by any other client (COMP-10 §1).
  COMP-10's surfaces are a closed list, and no client may ask the compositor to
  show one.
- Both roads to a new command pass through abyss: the socket write
  (`set_config_collection`) and a file edit picked up by the watcher. Both land
  in `config::apply_loaded`.
- abyss never runs widget commands; hyperion does, from what `get_config`
  serves it.

## Options
1. **Settings asks.** An ordinary client; imitable; a socket peer skips it.
2. **Hyperion asks before running.** Imitable, and a client asking itself.
3. **abyss withholds and asks.** abyss decides a widget is unapproved, keeps it
   out of the live config, and shows its own trusted surface. No client
   requests the prompt.

## Decision
We take **3**, as COMP-10 §3.11 *Command approval*.

**Premade catalog.** The hyperion package ships command widgets as
`/usr/share/eclipse/widgets/<name>.kdl`, argv-only, one `widget` block each.
While `taskbar-widgets` is on, abyss reads them as the lowest config layer,
before `/etc/eclipse/abyss.kdl`. Each catalog block is approved by virtue of
shipping: its hash is the reference.

**Hash.** blake3 over a canonical encoding of what runs: kind (`exec`/`stream`),
argv, `interval-ms`, and the `on-click`/`on-scroll-up`/`on-scroll-down` argv.
Not the KDL text, so formatting and ordering do not change it. Declarative
(`source`) widgets run nothing and are never withheld.

**Approvals.** `$XDG_STATE_HOME/eclipse/widget-approvals.kdl` maps a widget name
to the hash the owner accepted. Only the approval surface writes it.

**Withhold.** At `apply_loaded`, a command widget is live only if its hash
equals its catalog hash or its recorded approval. Otherwise it is withheld: kept
out of the live config (so hyperion never sees its argv), reported by
`get_config` as `approval: "pending"` by name only, and queued for a prompt. A
`custom:<name>` id in `bar.widgets.order` stays valid while withheld and draws
nothing.

**The prompt.** A compositor-drawn modal (COMP-10 §1, §4): it grabs the seat,
draws above everything, and is absent from capture. One widget per prompt.
- *Altered premade* (a user block shadowing a catalog name, different hash):
  "This widget has been altered! Altered widgets are not guaranteed to be
  safe!", the widget name, the command as plain text, **[Revert]** **[Accept]**.
  Revert removes the user's override so the catalog block applies again.
- *New command widget* (no catalog entry): "This is not a premade widget. It
  runs this command as you. EclipseOS is not responsible for what it does.",
  the name, the command, **[Remove]** **[Allow]**. Remove deletes the block.
- Both prompts also offer **[Not now]**, which changes no file and leaves the
  widget withheld. Default focus is on Not now, Escape means Not now, and Enter
  never means Accept/Allow (COMP-10 §3.2). Revert and Remove edit the owner's
  config, so they are never the reflexive answer either.
- The command is widget-supplied text: control characters stripped, clamped,
  plain text, in the untrusted block, never in the trusted position.
- Not now is remembered for that hash for the session; the prompt is not
  reshown until the definition changes again or the owner asks from Settings
  (which only re-queues; it cannot approve).

**Settings** shows the same disclaimer and paragraph in the custom widget
editor for any command widget that is not premade, marks catalog widgets
**Premade**, and shows a pending widget as waiting on approval, with a
**Review** control that re-queues its prompt. It cannot approve.

## Consequences
- The socket can still write a command widget, but it cannot make one run. This
  closes ADR 0065's open question.
- Hand edits to `abyss.kdl` are treated the same as socket writes: a new or
  changed command asks once.
- One new socket method, `review_widget` (gate kind `Command`, owner uid,
  bound to `taskbar-widgets`): re-queues a pending widget's prompt. It carries
  no answer, and a widget already queued or on screen is not queued twice, so a
  peer cannot stack prompts.
- abyss links `blake3` (already in the lockfile via policyd).
- New TCB code: `trusted_ui/` gains its first modal surface, the seat grab
  (`shell/focus.rs` `prompt_grab_active`), and the front-most render pass on
  every backend. Owner line-by-line review.
- COMP-10 §2's personal secret is not built yet, so the prompt ships without
  it. It is exactly as imitable as any other client window until the secret
  lands; what it already guarantees is that no client can skip it.
- Without the `taskbar-widgets` hook, none of this runs: no catalog, no
  withholding, no prompt, and the widget collection is not writable.

## Revisit when
- COMP-10 §2's personal secret lands: this surface adopts it.
- The pending-decision queue (COMP-10 §3.10 in code comments) is built:
  withheld widgets join it instead of prompting at once.
- A catalog widget needs a script rather than argv: the hash must then cover
  the script's bytes.
