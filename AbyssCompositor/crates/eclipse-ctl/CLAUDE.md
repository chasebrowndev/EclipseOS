# eclipse-ctl — control-socket CLI

Governing spec: COMP-13 §2.3. **Not TCB**: it is an ordinary client of the
control socket and holds no authority of its own — everything it can do, any
process running as the same user can do by writing JSON to the socket. The
gate lives in `abyss` (`crates/abyss/src/ipc/gate.rs`), never here.

- No dependencies beyond `serde_json` and `libc` (SIGPIPE reset only). Argument parsing is hand-rolled; a
  clap dependency on a tool this small is not worth the supply chain.
- SPDX `AGPL-3.0-only` on every file (it is a system tool, not an SDK).
- It must stay thin: no state, no caching, no retries that could mask a
  compositor that is wedged.
