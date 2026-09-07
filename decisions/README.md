# Decision log (F-08)

One decision per file, `NNNN-slug.md`, format in `TEMPLATE.md`. **Never edited
after acceptance except to change status** — superseding a decision means
writing a new ADR that says so.

| # | Decision | Status |
|---|---|---|
| 0001 | Rust as implementation language | accepted (not yet written up) |
| 0002 | [Custom compositor on Smithay, pinned `=0.7.0`](0002-smithay-not-a-fork.md) | accepted |
| 0003 | Custom Wayland protocol for agent control, not libei/portal | accepted (not yet written up) |
| 0004 | MCP-compatible agent surface from day one | accepted (not yet written up) |
| 0005 | [AGPLv3 + Apache-2.0 SDKs, CLA, commercial dual licence](0005-agpl-plus-commercial.md) | accepted |
| 0006 | No ISO-first; compositor first | accepted (not yet written up) |
| 0007 | [Agent seats default, compat lock fallback](0007-agent-seats-default.md) | accepted |
| 0008 | [Policy in-process from a compiled table; defer is tighten-only](0008-policy-in-process-table.md) | accepted |
| 0009 | [Trusted UI compositor-drawn, never layer-shell](0009-trusted-ui-compositor-drawn.md) | accepted |
| 0010 | WM mode only, no DE in v1 | accepted (not yet written up) |
| 0011 | Hyprland-style human UX; agent workspaces untiled | accepted (not yet written up) |
| 0012 | Default sensitivity private; app-declared class raise-only | accepted (not yet written up) |
| 0013 | Human keystrokes never logged by content | accepted (not yet written up) |
| 0014 | Cross-agent collaboration via channels with non-forgeable provenance | accepted (not yet written up) |
| 0015 | `registryd` outside TCB; never receives secret surfaces | accepted (not yet written up) |
| 0016 | [Configuration format is KDL](0016-kdl-config.md) | accepted |
| 0017 | [NVIDIA-first GPU baseline; explicit sync mandatory](0017-nvidia-first-gpu-baseline.md) | accepted |
| 0018 | [Single-threaded core owning `HeliosState`](0018-single-threaded-core.md) | accepted |
| 0019 | [Focus-follows-mouse default](0019-focus-follows-mouse-default.md) | accepted |
| 0020 | [Interim quit binding `Super+Shift+Q`](0020-quit-binding-super-shift-q.md) | accepted |
| 0021 | [Per-workspace arena binary tree for tiling](0021-layout-tree.md) | accepted |
| 0022 | [`wlr_data_control` behind a process-name allowlist](0022-data-control-allowlist.md) | accepted |
| 0023 | [Output identity by EDID, layouts persisted per output-set](0023-output-identity-and-persistence.md) | accepted |

Numbers 0001–0015 are the seeded list from F-08 §"Seeded ADRs"; the ones marked
"not yet written up" are decided in the spec and reserved here so numbering
stays stable. Write them up as they are touched.
