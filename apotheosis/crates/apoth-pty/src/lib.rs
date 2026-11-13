// SPDX-License-Identifier: AGPL-3.0-only
//! PTY allocation, child spawn, pidfd exit detection, foreground process info.
//!
//! Governing spec: T-18 §7 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![deny(unsafe_code)] // unsafe is confined to FFI modules, each with `#[allow(unsafe_code)]` and a SAFETY comment
