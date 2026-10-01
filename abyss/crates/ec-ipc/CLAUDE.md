# eclipse-ipc — control-socket client

Read the root `CLAUDE.md` first.

- **Not TCB.** This is the client half of COMP-13 §2: it asks and is refused.
  It holds no authority and must never grow a code path that assumes it does.
- No async runtime. The socket is a plain fd; every DE binary drives it from
  its own `calloop` loop via `Client::as_raw_fd`.
- Deps stay at `serde_json` + `libc`. A dependency here is a dependency in
  every shell binary.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
