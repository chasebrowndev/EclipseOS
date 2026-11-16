// SPDX-License-Identifier: AGPL-3.0-only
//! Font discovery, shaping, rasterisation and built-in glyphs.
//!
//! Governing spec: T-06 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). No GPU dependency: atlas packing
//! belongs to `apoth-render` (T-07); this crate produces coverage bitmaps and glyph placement.
#![forbid(unsafe_code)]

pub mod builtin;
mod builtin_tables;
mod colr;
pub mod config;
pub mod discovery;
pub mod embedded;
pub mod face;
pub mod metrics;
pub mod ranges;
pub mod raster;
pub mod shape;
pub mod system;
pub mod text;
