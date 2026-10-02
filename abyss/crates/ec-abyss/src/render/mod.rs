// SPDX-License-Identifier: AGPL-3.0-only
//! Render: everything state-free lives in `ec-abyss-render` and is re-exported
//! here so `crate::render::` paths keep working. `capture.rs` stays in this
//! crate: it reads `AbyssState` and is TCB.

pub use ec_abyss_render::*;

pub mod capture;

#[cfg(test)]
mod scan_tests;
