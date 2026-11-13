// SPDX-License-Identifier: Apache-2.0
//! Generated Unicode tables: cell width, grapheme cluster break, emoji presentation.
//!
//! Governing spec: T-04 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! The crate supplies primitives; policy (mode 2027, method switching, cursor-move resets)
//! lives in `apoth-term`:
//!
//! - [`Segmenter`]: streaming UAX #29 segmentation with an explicit [`Segmenter::reset`].
//! - [`Widths`], [`ClusterMeasure`]: cluster width under the `unicode` or `wcswidth` method,
//!   with `unicode.ambiguous-width` and `font.cell-width-override` as input.
//! - [`props`] accessors for the generated three-stage table.
//!
//! Unicode version overlay (`unicode.version`, R2, UNICODE-05) is a seam only:
//! [`Widths`] is the one place a per-version overlay would hook in.
#![forbid(unsafe_code)]

#[doc(hidden)]
pub mod generator;
pub mod props;
pub mod segment;
#[rustfmt::skip]
mod tables;
pub mod width;

pub use props::{class_of, unicode_version};
pub use segment::{Class, Graphemes, Segmenter, graphemes};
pub use width::{
    Ambiguous, CellWidthOverrides, ClusterMeasure, MAX_CLUSTER_SCALARS, OverrideError, VS15, VS16,
    WidthMethod, Widths,
};
