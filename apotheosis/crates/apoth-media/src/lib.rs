// SPDX-License-Identifier: AGPL-3.0-only
//! Media placements: protocol, the sandboxed decoder helper, A/V sync.
//!
//! Governing spec: T-16 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![deny(unsafe_code)] // unsafe is confined to FFI modules, each with `#[allow(unsafe_code)]` and a SAFETY comment
