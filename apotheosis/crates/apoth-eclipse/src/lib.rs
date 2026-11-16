// SPDX-License-Identifier: AGPL-3.0-only
//! EclipseOS integration: Abyss detection, theme, semantic publishing, sensitivity.
//!
//! Governing spec: T-17 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). Everything here is
//! additive: with detection negative Apotheosis is a complete terminal anywhere.
#![forbid(unsafe_code)]

pub mod detect;
pub mod theme;

pub use detect::{Abyss, Backoff};
pub use theme::{Rgb8, TermColors};
