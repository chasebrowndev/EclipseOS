// SPDX-License-Identifier: AGPL-3.0-only
//! Shared policy types for EclipseOS.
//!
//! Two processes link this crate: `policyd`, which mints tasks and signs
//! grants, and `abyss`, which verifies them and evaluates the enforcement
//! table in-process (ADR 0008, COMP-11 §1). One definition of a grant, one
//! definition of a task state, one canonical encoding — because a policy
//! decision that depends on which process parsed the bytes is not a policy
//! decision.
//!
//! The crate is TCB. It deliberately has no I/O, no threads, no clock and no
//! entropy source: every function takes what it needs as an argument, so the
//! same inputs always produce the same answer and the tests can say so.

#![forbid(unsafe_code)]

pub mod cbor;
pub mod grant;
pub mod task;

pub use grant::{Capability, Constraints, Grant, Rate, VerifyError};
pub use task::{CloseReason, Counters, Origin, RateRing, Task, TaskEvent, TaskState, Ulid};
