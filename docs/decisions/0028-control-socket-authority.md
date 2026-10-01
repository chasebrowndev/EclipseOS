# 0028 — The control socket is owner-only, and window titles cross it
Status: accepted
Date: 2026-09-07
Deciders: chase (owner), Claude (advisory)

## Context
COMP-13 gives the human a JSON-RPC control socket so a bar, a launcher and
`eclipse-ctl` can drive the compositor. That socket is a capability surface: it
reads window state and mutates focus, workspaces and floating. It cannot be
ambient authority (root invariant), and it must not become a side channel for
human input (root invariant: keystrokes, clipboard and IME are never logged by
content).

Two forces pull against each other. A bar is useless without window titles —
that is the text it renders. But a title is partly human input: a browser puts
the URL bar in it, an editor puts the filename, and a password manager puts the
name of the entry being viewed. A title is not a keystroke log, but it is not
nothing either.

The socket also has to survive a compositor that has not finished growing. Half
the interesting methods depend on subsystems that land in Phase 2.

## Options
1. No titles over IPC. Safest, and makes the socket useless for its one
   stated exit gate (waybar driven by our IPC).
2. Titles to everyone who can open the socket. Simple; makes the title
   readable by anything that gets a file descriptor.
3. Titles, but the socket is owner-only and every method crosses a
   fail-closed table first. Keeps the exit gate reachable and puts the
   disclosure behind the same uid check as the session itself.

## Decision
Option 3. The socket lives at `$XDG_RUNTIME_DIR/eclipse/abyss.sock` with the
directory 0700 and the socket 0600, so only the session owner's uid can connect
at all. Every request crosses `ipc::gate::check` before any state is read or
written: a method with no row in `TABLE` does not exist and is denied, and the
decision is a ratchet — `Decision::tighten` turns `Allow` into `Deny` and never
the reverse. Methods are classed `Query`, `Command`, `Privileged` or
`ScriptedInput`; `ScriptedInput` synthesises human input and is off unless the
config turns it on. A method whose subsystem has not landed carries
`implemented: false` and answers "not implemented" rather than pretending to
have succeeded.

Window titles are returned by `get_windows`, `get_focused` and the event
stream, to the owner only. Keystrokes, clipboard contents and IME preedit are
not exposed by any method, and no metric is derived from them.

## Consequences
- The exit gate is reachable: a bar can render titles over our own IPC.
- Anything running as the owner's uid can read every window title. That is a
  real disclosure and it is deliberate; it is the same trust boundary that
  already lets a process read the owner's files.
- The socket is a single-threaded calloop event source, so it holds no lock on
  `AbyssState` and cannot stall the hot path.
- We owe a per-peer identity story. Right now the uid check is the whole
  authorisation; there is no way to give a bar read-only access while denying
  it `close_window`. The `Kind` classes exist so that can be added without
  reshaping the table.
- `Privileged` is specified but has no rows until the emergency panel lands.

## Revisit when
- A non-bar consumer wants the socket, at which point per-peer identity stops
  being optional and this ADR gets superseded.
- Sensitivity classes land (COMP-16 M13): a title on a surface classed
  `secret` should almost certainly be withheld or redacted, which this ADR
  currently does not do.
