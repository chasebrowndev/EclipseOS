# 0066 — Add-ons: one kind, a manifest, and hooks that stay off
Status: accepted
Date: 2026-09-26
Deciders: chase (owner), Claude (advisory)

## Context
Three optional pieces of EclipseOS are shaped differently today:
- **Oracle Eyes** is an out-of-process daemon holding two revocable grants: the
  `annotation_*` socket methods and a `capture` line in `policy.kdl` (ADR 0041,
  COMP-18). The compositor keeps its half (the annotation pass, the region
  selector) built in and inert.
- **fog-activity** is a build variant: `fog-agentic` is Fog compiled with
  `--features activity`, a package that provides and replaces `fog`
  (FOG-SPEC, "Agent activity").
- **hyperion**, the taskbar, is a hard dependency of `eclipseos-meta`, and it is
  the only thing that registers the BlueZ pairing agent (ADR 0053). Command
  widgets (ADR 0065) need compositor-side support that should not exist on a
  system without a taskbar.

Nothing defines "add-on", so each one invents its own packaging, its own
on/off story and its own authority. The owner wants one kind of add-on, and
wants an add-on's compositor-side behaviour to arrive with the add-on rather
than a compositor that carries every add-on's code switched on.

Forces:
- ADR 0041 rejected loading add-on code into abyss (no stable Rust ABI, ambient
  authority, a crash surface inside the single-threaded core). That stands.
- A WASM runtime would ship compositor code with the add-on, but it puts a
  large runtime and foreign code in the TCB, in the core's loop, and would let
  add-on code draw where only the compositor may (Trusted UI).
- Prebuilt compositor variants per add-on multiply with every combination, and
  each variant is TCB to review and test.
- Wayland gives one app no way to draw inside another's window, which is why
  Fog chose a build variant.

## Options
1. **Keep both shapes** (out-of-process and build variant). Two kinds of add-on,
   two packaging stories. Rejected by the owner.
2. **WASM modules loaded by hosts.** Real code shipped with the add-on; wasmtime
   in the TCB; Trusted UI invariant bent.
3. **Prebuilt host variants per add-on.** Combinatorial; every variant is TCB.
4. **One kind: a manifest plus hooks.** Every add-on is a package with a
   manifest. Hosts carry small, generic hooks that do nothing until an
   installed manifest turns them on. Add-on code always runs in its own process.

## Decision
We take **4**.

**An add-on** is an optional package that ships its programs, its user units,
and one manifest at `/usr/share/eclipse/addons/<id>.kdl`:

```kdl
id "oracle-eyes"
name "Oracle Eyes"
hooks "annotations" "region-select"
capture "request"   // shown to the owner; granted only in policy.kdl
```

**Hooks.** A hook is a named, generic behaviour in a host (abyss, Fog) that is
off unless at least one installed manifest names it. Hosts ask "is hook X on?",
never "is add-on Y installed?". A hook is small and does nothing on its own; the
add-on's process is what drives it.

**Manifest trust.** Hosts read manifests only from `/usr/share/eclipse/addons/`,
which is package-owned and root-writable. User directories are never read, so a
user-level process cannot turn a hook on. Hosts re-read the directory on change
(a package install or removal) and apply the result live. A malformed manifest
is skipped and logged; it enables nothing.

**Authority.** A hook gates what already exists; it grants nothing new.
- Socket methods bound to a hook are refused while the hook is off, in addition
  to the static fail-closed gate table (`ipc/gate.rs`). An add-on still needs
  the owner uid and the method's gate kind.
- Capture is never enabled by a manifest. `capture "request"` only lets
  Settings show that the add-on wants it; the owner grants it in `policy.kdl`
  (COMP-18 §3).
- An add-on never draws Trusted UI. A hook may cause the **compositor** to draw
  one of its own trusted surfaces (ADR 0067); the add-on supplies no pixels and
  no text in the trusted position.

**Selection.** An add-on is installed by its package. Install profiles and
component slots (ADR 0060, ADR 0062, where merged) choose which add-on packages
a system gets; nothing else turns an add-on on.

**Hooks at v1.**

| Hook | Host | Turns on |
|---|---|---|
| `annotations` | abyss | `annotation_*` methods; annotation binds forward on `keybind` |
| `region-select` | abyss | the `region-select` bind action |
| `taskbar-widgets` | abyss | the `widget` collection and its socket writes, the premade widget catalog, command approval (ADR 0067) |
| `activity-lens` | Fog | the lens pill bar and agent views, fed by the add-on's daemon |
| `agents` | abyss | the privileged `ec-agent.sock` socket, agent and `eclipse_semantic_v1` globals, the `policyd` link, the "agents disabled" indicator, the agent lifecycle methods (`get_agents`, `pause_agent`, `resume_agent`, `terminate_agent`, `revoke_grants`) (ADR 0069) |

**The add-ons.**
- **hyperion** enables `taskbar-widgets`. `eclipseos-meta` lists it as an
  optional dependency, not a hard one. The pairing agent moves out to
  `eclipse-pairing`, a small eclipse-services binary with its own user unit,
  shipped with `eclipse-secret-prompt` in `eclipseos-desktop`, so Bluetooth
  pairing works without a taskbar. The StatusNotifierWatcher stays in hyperion:
  without a tray host nothing needs it.
- **Oracle Eyes** enables `annotations` and `region-select` and requests capture.
- **fog-activity** ships `fog-activityd` (the `agentd` activity subscription,
  the replay log and the `fog.*` MCP bridge) and enables `activity-lens`. Fog
  carries the lens views and shows no pill bar while the hook is off. Fog
  itself links no `agentd` client. The `fog-agentic` build variant is withdrawn.
  The package depends on `eclipseos-agents` (ADR 0069).

## Consequences
- abyss gains a manifest loader and a hook set on `AbyssState`, the gate gains
  a hook check, `get_config` reports installed add-ons, and `eclipse-ctl addons`
  lists them. Fog gains the same loader for its own hooks.
- The annotation binds, the region selector and the widget collection keep their
  code but do nothing on a system without the add-on. This is the "no bloat"
  line: the host keeps a switched-off hook, never an add-on's logic.
- `eclipseos-hyperion` is no longer required. A session without it starts;
  Super+E/R/N still reach the launcher and the center (built-in binds).
- `eclipse-pairing` and `eclipse-secret-prompt` move to `eclipseos-desktop`.
- COMP-18 §3 and FOG-SPEC ("Agent activity") are amended to match. ADR 0041's
  decision stands and is now one instance of this one.
- Adding a hook is a host change with its own review; an add-on cannot add one.

## Revisit when
- An add-on needs behaviour in a host that no generic hook can express without
  carrying that add-on's logic: that is the plugin question (ADR 0041) again.
- A third party wants to ship an add-on: manifest signing and a review step
  (COMP-10 §3.8 install review) come into scope.
