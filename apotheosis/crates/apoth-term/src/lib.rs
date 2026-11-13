// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic terminal: feed(bytes) -> grid state, PTY replies and events.
//!
//! Governing spec: T-03 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![forbid(unsafe_code)]
