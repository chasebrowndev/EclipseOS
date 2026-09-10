# abyss — the EclipseOS compositor

`abyss` is a Wayland compositor written in Rust on
[Smithay](https://github.com/Smithay/smithay) (pinned `=0.7.0`). It is the
window manager for EclipseOS and half of its trusted computing base: agents
drive the desktop through a compositor-native protocol, under a policy check
that runs before any state changes.

**Status: early.** Phase 1 milestone 1 — winit backend, one xdg toplevel,
keyboard and pointer. Not usable as a daily driver yet.

## What makes it different

- **Agents get their own Wayland seats.** Agent input never races or steals the
  human's focus, and every event is attributable to a principal (ADR 0007).
- **No ambient authority.** Every agent operation crosses a capability check
  against a compiled policy table, in-process, fail-closed (ADR 0008).
- **Trusted UI is drawn by the compositor**, above every client, so consent
  prompts and the activity indicator cannot be spoofed (ADR 0009).
- **Human input is never logged by content.** Not at any log level, not in the
  audit trail.

## Quick start

```
cargo build --workspace
cargo run -- --backend winit     # nested in your current session
journalctl --user -t abyss -f   # logs
```

`Super+Shift+Q` quits. Full instructions and system dependencies:
[docs/BUILDING.md](docs/BUILDING.md).

## Documentation

| | |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | crate layout, module map, trust boundaries |
| [docs/BUILDING.md](docs/BUILDING.md) | dependencies, build, run, logs |
| [decisions/](decisions/) | architecture decision records |
| [CLAUDE.md](CLAUDE.md) | invariants (for humans and AI assistants alike) |
| `ECLIPSEOS_SPECS_v2_VOL1.md` | the specification bundle; authoritative |

## Licence

System code is **AGPL-3.0-only** ([LICENSE](LICENSE)). Protocol definitions and
the agent SDKs are **Apache-2.0** ([LICENSE-APACHE](LICENSE-APACHE)) so agents
may be written under any licence. A commercial licence is available for
organisations that cannot comply with the AGPL — see
[CONTRIBUTING.md](CONTRIBUTING.md) and [NOTICE](NOTICE). The name and logo are
not licensed by the AGPL: [TRADEMARK.md](TRADEMARK.md).
