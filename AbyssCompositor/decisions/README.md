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
| 0018 | [Single-threaded core owning `AbyssState`](0018-single-threaded-core.md) | accepted |
| 0019 | [Focus-follows-mouse default](0019-focus-follows-mouse-default.md) | accepted |
| 0020 | [Interim quit binding `Super+Shift+Q`](0020-quit-binding-super-shift-q.md) | accepted |
| 0021 | [Per-workspace arena binary tree for tiling](0021-layout-tree.md) | accepted |
| 0022 | [`wlr_data_control` behind a process-name allowlist](0022-data-control-allowlist.md) | accepted |
| 0023 | [Output identity by EDID, layouts persisted per output-set](0023-output-identity-and-persistence.md) | accepted |
| 0024 | [A locker crash leaves the session locked behind a compositor fallback](0024-session-lock-fallback.md) | accepted |
| 0025 | [Direct scanout is off for any frame containing a sensitive surface](0025-direct-scanout-and-redaction.md) | accepted |
| 0026 | [XWayland is one untrusted trust domain, its clipboard a focus-scoped grant](0026-xwayland-trust-domain.md) | accepted |
| 0027 | [Screen capture is denied unless the client is on a process-name allowlist](0027-screencopy-fail-closed.md) | accepted |
| 0028 | [The control socket is owner-only, and window titles cross it](0028-control-socket-authority.md) | accepted |
| 0029 | [`xdg-desktop-portal-wlr` is allowlisted by the user, never by default](0029-portal-capture-allowlist.md) | accepted |
| 0030 | [Two capture protocols coexist, behind one shared gate](0030-ext-image-copy-capture.md) | accepted |
| 0031 | [Resizing a window over the control socket](0031-resize-over-the-control-socket.md) | accepted |
| 0032 | [The systemd session handoff lives in the compositor, behind `--session`](0032-session-handoff-in-compositor.md) | accepted |
| 0033 | [An explicit render-device that does not resolve refuses to start](0033-render-device-override-refuses-on-miss.md) | accepted |
| 0034 | [Terminal emulator: keep the foot fork, extract the publisher as `cataclysm-pub`](0034-terminal-foot-fork-publisher-crate.md) | accepted |
| 0035 | [Day-one CI: cloud-only gate, `cargo-deny` interim, mechanized review rules](0035-ci-harness-day-one.md) | accepted |
| 0036 | [Config writes byte-splice the file; the KDL document model does not](0036-config-writes-byte-splice.md) | accepted |
| 0037 | [The security surface moves to its own file, `policy.kdl`](0037-abyss-kdl-policy-kdl-split.md) | accepted |
| 0038 | [The DE userland is native Rust, not Quickshell](0038-de-userland-is-native-rust.md) | accepted |
| 0039 | [Supply-chain exceptions for the iced toolkit](0039-iced-supply-chain-exceptions.md) | accepted |
| 0040 | [Annotation overlays are a compositor pass of their own, not trusted UI](0040-annotation-overlay-pass.md) | accepted |
| 0041 | [Oracle-Eyes runs out of process, on two revocable capabilities](0041-oracle-eyes-out-of-process.md) | accepted |
| 0042 | [Focused output is first-class; keyboard focus derives from it](0042-focused-output-and-pointer-focus.md) | accepted |
| 0043 | [The benchmark harness is hand-rolled, not criterion](0043-hand-rolled-bench-harness.md) | accepted |
| 0044 | [One canonical CBOR profile, hand-rolled, for grants, audit and sockets](0044-canonical-cbor-profile.md) | accepted |
| 0045 | [Grants are COSE_Sign1 over canonical CBOR, Ed25519, embedded payload](0045-grant-signing-cose-sign1.md) | accepted |
| 0046 | [The S-04 §4 audit store is the task/counter journal, built at milestone 10](0046-shared-audit-store-at-m10.md) | accepted |
| 0047 | [The `policy-eval` API surface: what the two processes are allowed to share](0047-policy-eval-api-surface.md) | accepted |
| 0048 | [A task's statement is journalled as a hash, and does not survive a restart](0048-task-statement-is-hashed-not-journalled.md) | accepted |
| 0049 | [Outputs carry a number; Ctrl+Super+[N] moves a window to display N](0049-output-number-and-move-to-output.md) | accepted |
| 0050 | [Oracle-Eyes may call `get_outputs`](0050-oracle-eyes-reads-get-outputs.md) | accepted |
| 0051 | [An `org.freedesktop.ScreenSaver` service bridges D-Bus idle inhibits to the compositor](0051-screensaver-service-bridges-idle-inhibit.md) | accepted |
| 0052 | [One crate and one package per swappable DE component](0052-one-crate-per-swappable-de-component.md) | accepted |
| 0053 | [The status service gains actions; secrets go through their own prompt process](0053-status-actions-and-secret-prompt.md) | accepted |
| 0054 | [Annotations gain a title and a pick marker](0054-annotation-pick-marker.md) | accepted |

Numbers 0001–0015 are the seeded list from F-08 §"Seeded ADRs"; the ones marked
"not yet written up" are decided in the spec and reserved here so numbering
stays stable.
