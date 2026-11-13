# 0122 — No embedded scripting or plugin runtime
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
kitty has Python kittens, wezterm has Lua, zellij has WASM plugins. They buy unbounded control at the cost of a second language, an unstable API surface, and code execution from config.

## Options
1. Embed Lua or WASM.
2. A closed, typed `Action` vocabulary; declarative rules (`event → actions`), macros, profiles, layouts, templates and a match language; a JSON control socket for anything else.

## Decision
Option 2 (T-11, T-12). Whatever a script would do in-process, an external program does over the socket in any language. `spawn`/`pipe` take argv arrays with typed placeholders, never shell strings.

## Consequences
Config can be validated completely and shown in a GUI. The templates and match language must stay small (no loops, no assignment); each has a grammar and a fuzz target. Rules triggered by program output need explicit per-rule opt-in to inject input or spawn.

## Revisit when
A real use case cannot be expressed by rules plus an external socket client without unacceptable latency.
