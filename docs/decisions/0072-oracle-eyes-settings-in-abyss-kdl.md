# 0072 — Oracle Eyes' settings live in abyss.kdl; a changed model command needs approval
Status: accepted
Date: 2026-10-02
Deciders: chase (owner), Claude (advisory)

## Context
The owner wants Oracle Eyes in Settings: its own section with editable
settings, a debug toggle, keybinds, colours and its capture status. COMP-17 §3
lets Settings write `abyss.kdl` and nothing else, and the daemon's settings
lived in `oracle-eyes.toml`. One setting is a command line the daemon executes,
and oracle-eyes/spec.md §3.4 makes the locked-down model invocation (no tools,
one turn, a fixed reply schema) a hard requirement.

## Options
1. Let Settings write `oracle-eyes.toml`: amends COMP-17 §3 and gives Settings
   a second file to own.
2. Move the settings into an `oracle-eyes { … }` block in `abyss.kdl`, served to
   the daemon by `get_config` and its config-changed event.

## Decision
Option 2. Settings stays a client of the COMP-13 §1.4 write API only, with
schema-generated controls; the daemon reads the block over the socket and
applies changes live, falling back to `oracle-eyes.toml` only without a
compositor. `debug` in the block joins `--debug`/`OE_DEBUG` (any on is on). The
model command is editable but (a) the daemon always appends the locked flags
itself, so no configured command can drop them, and (b) a command that differs
from the default or the owner's approved one is withheld from `get_config` and
queued for the COMP-10 §3.11 prompt, reusing ADR 0067's withhold path. Capture
status is shown read-only by reading `policy.kdl` from disk (ADR 0038); the
socket stays closed to policy. Amends COMP-17 §3 (Appendix G-05).

## Consequences
- Add-on settings have a precedent: an add-on's own block in `abyss.kdl`.
- The daemon owes a test that the locked flags survive any configured command.
- The §3.11 prompt now presents a second kind of command.

## Revisit when
A second add-on wants Settings controls — then decide whether add-on blocks
become manifest-declared schema.
