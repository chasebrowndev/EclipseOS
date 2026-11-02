// SPDX-License-Identifier: AGPL-3.0-only
//! Geometry specific to the portal dialogs.
//!
//! Colours, radii, type sizes and the shared spacing all come from
//! `ec_ui::tokens`. What is here is only the size of a dialog window and of
//! the few objects only these panes have (a source tile, its thumbnail), kept
//! in one place so no view carries a literal. If a second crate ever needs
//! one, it moves to `ec_ui::tokens`.

/// A dialog window: width and height, logical px. Fixed, so abyss floats it
/// without a rule, like `ec-secret-prompt`.
pub const SOURCE_W: f32 = 720.0;
pub const SOURCE_H: f32 = 640.0;
pub const SHEET_W: f32 = 480.0;
pub const SHEET_H: f32 = 380.0;
pub const SHOT_W: f32 = 600.0;
pub const SHOT_H: f32 = 400.0;
pub const COLOR_W: f32 = 540.0;
pub const COLOR_H: f32 = 480.0;
pub const ACCOUNT_W: f32 = 500.0;
pub const ACCOUNT_H: f32 = 420.0;

/// The requester's icon square in the header.
pub const AVATAR: f32 = 52.0;
/// The larger avatar in the account sheet.
pub const AVATAR_LARGE: f32 = 72.0;
/// Initial letter in a placeholder avatar.
pub const AVATAR_INITIAL: f32 = 22.0;
/// Between the avatar and the title block.
pub const AVATAR_GAP: f32 = 16.0;

/// Tiles per row in the source grid, and how tall the grid's viewport is
/// (two rows and a hint of the third, so a long list shows it scrolls).
pub const GRID_COLS: usize = 3;
pub const GRID_H: f32 = 330.0;
/// A tile's thumbnail well.
pub const THUMB_H: f32 = 92.0;
/// Padding inside a tile, around the well and the caption.
pub const TILE_PAD: f32 = 8.0;
/// Between a tile's well and its caption lines.
pub const TILE_GAP: f32 = 8.0;
/// A tile's selection ring.
pub const RING: f32 = 1.5;
/// The dot that says a tile is selected, besides its colour.
pub const DOT: f32 = 8.0;

/// The placeholder monitor and window drawn in a thumbnail well.
pub const GLYPH_SCREEN_W: f32 = 56.0;
pub const GLYPH_SCREEN_H: f32 = 34.0;
pub const GLYPH_WINDOW_W: f32 = 44.0;
pub const GLYPH_WINDOW_H: f32 = 30.0;
pub const GLYPH_BAR_H: f32 = 5.0;
pub const GLYPH_STROKE: f32 = 1.5;
/// Air between the screen outline and the region drawn in it.
/// The region drawn inside the screen outline, as a fraction of it.
pub const GLYPH_REGION: f32 = 0.5;
pub const GLYPH_INSET: f32 = 6.0;

/// A screenshot mode card's well, taller than a tile's: three cards share a row.
pub const MODE_WELL_H: f32 = 110.0;

/// The colour pane's live swatch.
pub const SWATCH_W: f32 = 150.0;
pub const SWATCH_H: f32 = 88.0;
/// The hex field.
pub const HEX_W: f32 = 150.0;

/// How far content settles in when a dialog opens, px. Skipped with reduce-motion.
pub const ENTER_DROP: f32 = 10.0;
/// The most characters of requester-supplied text a dialog will lay out.
pub const MAX_NAME: usize = 28;
pub const MAX_ID: usize = 30;
pub const MAX_DETAIL: usize = 280;
pub const MAX_TITLE: usize = 80;
/// A permission's title as it reads in the subtitle.
pub const MAX_SUBJECT: usize = 40;
pub const MAX_ADDRESS: usize = 64;
/// Addresses listed before "and N more".
pub const MAX_ADDRESSES: usize = 4;
/// Longest window title or output name in a tile caption.
pub const MAX_CAPTION: usize = 26;
