// SPDX-License-Identifier: AGPL-3.0-only
//! Accessibility: AccessKit tree builder and AT-SPI adapter.
//!
//! Governing spec: T-20 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). The tree builder ([`tree`])
//! and text model ([`text`]) are pure and tested without D-Bus; [`adapter`] is the
//! `accesskit_unix` transport.
#![forbid(unsafe_code)]

pub mod adapter;
pub mod ids;
pub mod text;
pub mod tree;

pub use accesskit;
pub use adapter::{Adapter, Notify, Request};
pub use text::RowText;
pub use tree::{
    BarButtonModel, BarModel, Builder, ChipModel, PaletteModel, PaneModel, ProgressKind,
    ProgressModel, SearchModel, TabModel, TextModel, TooltipModel, WindowModel,
};
