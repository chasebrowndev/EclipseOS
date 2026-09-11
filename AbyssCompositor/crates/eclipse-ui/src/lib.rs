// SPDX-License-Identifier: AGPL-3.0-only
//! The Eclipse design system.
pub mod theme;
pub mod tokens;
pub mod widget;

/// Register the vendored UI and data faces.
///
/// Call this once, from the application builder, before any pane is drawn:
/// `iced::application(..).font(eclipse_ui::FONTS[0])` and so on, or feed the
/// whole slice. Doing it here as a side effect would be once per process and
/// impossible to opt out of.
pub const FONTS: &[&[u8]] = tokens::font::BYTES;
