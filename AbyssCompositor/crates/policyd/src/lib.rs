// SPDX-License-Identifier: AGPL-3.0-only
//! `policyd` — the EclipseOS policy daemon (COMP-16 milestone 10).
//!
//! Holds the task store (A-04), mints and revokes grants (S-01 §4), and owns
//! the durable audit store (S-04 §4). Single-threaded by construction: the
//! task store is the authority on what exists, and an authority with two
//! writers is not one.
//!
//! The daemon is a library with a thin `main` on top so that the store can be
//! exercised by tests — and, at milestone 11, by the socket layer — without
//! going through a process boundary.

#![forbid(unsafe_code)]

pub mod audit;
pub mod tasks;
