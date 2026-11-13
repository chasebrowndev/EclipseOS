// SPDX-License-Identifier: AGPL-3.0-only
//! Wayland client: windows, seats, scale, clipboard, IME, optional protocols by capability.
//!
//! Governing spec: T-09 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![deny(unsafe_code)] // unsafe is confined to FFI modules, each with `#[allow(unsafe_code)]` and a SAFETY comment
