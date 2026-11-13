// SPDX-License-Identifier: Apache-2.0
//! Streaming VT parser: ECMA-48/DEC state machine, string sinks with per-protocol byte budgets, bulk printable fast path.
//!
//! Governing spec: T-01 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![forbid(unsafe_code)]
