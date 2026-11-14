// SPDX-License-Identifier: AGPL-3.0-only
//! PTY allocation, child spawn, pidfd exit detection, foreground process info.
//!
//! Governing spec: T-18 §7 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`), T-00 §4.2 (write path),
//! T-18 §5 / ADR 0115 (credential signal), T-18 §10 (remote detection).
//!
//! Policy that stays with the caller: resize coalescing (25 ms idle), sampling schedule
//! for termios / foreground queries, logging of errors (this crate has no logger and
//! never logs content), shell resolution (CONFIG-41), the reply queue (64 KiB, T-18 §5).
#![deny(unsafe_code)] // unsafe is confined to `spawn`, with `#[allow(unsafe_code)]` and a SAFETY comment

mod env;
mod outq;
mod procinfo;
pub mod shell;
mod spawn;

pub use env::{EnvSpec, build_env, choose_term, terminfo_dirs, terminfo_exists};
pub use outq::{Enqueue, OUT_QUEUE_BOUND, OutQueue};
pub use procinfo::{
    BUILTIN_REMOTE_NAMES, MAX_WALK_DEPTH, MAX_WALK_PROCS, ProcFs, ProcSource, StatInfo, cwd,
    is_remote, parse_stat,
};
pub use spawn::{CloseTimings, ExitStatus, Pty, PtySize, SpawnConfig, TermiosState};
