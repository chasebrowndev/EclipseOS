# ec-abyss-wire — control-socket wire and gate types

Read the root `CLAUDE.md` first. Governing spec: COMP-13 §2.

**TCB status: not under the listed TCB paths, but `gate.rs` is the control
socket's authorisation gate (method table, owner-uid check, config-file and
add-on-hook ratchet). Treat edits to it as enforcement-path changes and get
owner review; the owner decides whether it formally joins the TCB.**

- Extracted from `ec-abyss/src/ipc/` and `addons.rs`. **No smithay, no calloop,
  no `AbyssState`.** Deps: `ec-abyss-config` (for `Config`) and `serde_json`.
- `gate.rs`: `TABLE`, `check`, `Decision` (ratchet: `tighten` only removes
  access), `Peer`, config-file and hook checks. Fail-closed: no row = deny.
- `rpc.rs`: `RpcError` and the JSON-RPC error codes.
- `events.rs`: `EVENTS`, the subscribable event kinds. `ec-ipc` does not
  mirror it.
- `hooks.rs`: `Hook` / `HookSet` (ADR 0066), re-exported by `ec-abyss::addons`.
- `ec-abyss` re-exports these under `crate::ipc::{gate,...}`; `ipc/gate.rs`
  there is a one-line shim.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
