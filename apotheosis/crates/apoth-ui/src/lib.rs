// SPDX-License-Identifier: AGPL-3.0-only
//! GPU-free glass toolkit: layout, hit-testing, focus, widgets -> draw list.
//!
//! Governing spec: T-08 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). The crate has no
//! blur, glow, bloom or gradient primitive: a surface cannot violate STYLE.md.
//! All coordinates are logical pixels; the renderer applies the output scale.
#![forbid(unsafe_code)]

pub mod bar;
pub mod draw;
pub mod frame;
pub mod geom;
pub mod hit;
pub mod mediawin;
pub mod motion;
pub mod palette;
pub mod pane;
pub mod scrollbar;
pub mod searchbar;
pub mod state;
pub mod tabs;
pub mod text;
pub mod toggle;
pub mod tokens;
pub mod tooltip;

pub use draw::{Cmd, DrawList, IconId, Rim};
pub use geom::{Point, Rect};
pub use hit::{HitTree, Layer};
pub use state::{ControlState, UiState, WidgetId};
pub use text::{FixedAdvance, Measure, TextOrigin, TextRun};
pub use tokens::{Palette, Rgba};

#[cfg(test)]
mod lint_tests;
#[cfg(test)]
mod tests;
