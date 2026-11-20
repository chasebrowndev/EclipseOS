# ec-layout — tile layout on a snapped grid

Read the root `CLAUDE.md` first. Governing decision: ADR 0126. **Linked by TCB
(`ec-greeter`), so treat it as TCB-adjacent: line-by-line review.**

- Pure: no iced, no I/O, no dependencies. Keep it that way; a root helper links it.
- A layout is valid or rejected whole. `validate` is the only gate for anything read
  from disk; `place` and `resize` only ever return valid spots.
- `Users` and `Password` are `REQUIRED`: no layout may hide the way in.
- `Kind::needs_session` kinds (now playing) must never reach the pre-auth greeter.
