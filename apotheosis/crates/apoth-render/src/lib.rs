// SPDX-License-Identifier: AGPL-3.0-only
//! wgpu renderer: atlases, cell/image/glass pipelines, frame scheduler, offscreen goldens.
//!
//! Governing spec: T-07 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![deny(unsafe_code)] // unsafe is confined to FFI modules, each with `#[allow(unsafe_code)]` and a SAFETY comment
