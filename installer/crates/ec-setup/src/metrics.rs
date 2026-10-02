// SPDX-License-Identifier: AGPL-3.0-only
//! Sizes the wizard needs that `ec_ui::tokens` has no name for.
//!
//! The wizard is a full-screen toplevel, not a settings pane, so a few of its
//! measures (the width of the column it centres, how tall a hero is) have no
//! home in the shared tokens. They live here rather than inline in a view. Any
//! that a second app ends up wanting moves to `tokens.rs`.

/// The content column is centred at this width on a wide screen and shrinks
/// with the window on a narrow one.
pub const CONTENT_W: f32 = 620.0;

/// The progress indicator: one short bar per step.
pub const PROGRESS_W: f32 = 16.0;
pub const PROGRESS_H: f32 = 3.0;
pub const PROGRESS_GAP: f32 = 5.0;

/// A hero banner: the accent or danger bordered band.
pub const BANNER_BORDER: f32 = 1.0;
pub const BANNER_RULE_W: f32 = 3.0;

/// One access point / disk / stage row.
pub const ROW_H: f32 = 40.0;
/// A stage of the install ladder.
pub const STAGE_H: f32 = 30.0;
/// The square that marks a stage as done, current or waiting.
pub const STAGE_MARK: f32 = 9.0;

/// A disk card's partition bar.
pub const DISK_BAR_H: f32 = 14.0;
/// Air between two partitions on the bar, so adjacent ones stay two shapes.
pub const DISK_BAR_GAP: f32 = 2.0;
/// The smallest a partition may draw, so a 512 MiB ESP on a 2 TB disk is
/// still a visible sliver rather than a missing one (out of 1000 portions).
pub const DISK_MIN_PORTION: u16 = 6;
pub const DISK_PORTIONS: f32 = 1000.0;

/// The signal-strength mark: four bars of rising height.
pub const SIGNAL_BAR_W: f32 = 3.0;
pub const SIGNAL_BAR_GAP: f32 = 2.0;
pub const SIGNAL_HEIGHTS: [f32; 4] = [4.0, 7.0, 10.0, 13.0];

/// A profile card.
pub const CARD_H: f32 = 158.0;
/// The word ("coming", "preview") in the corner of a card that is not ready.
pub const CARD_TAG_Y: f32 = 3.0;
pub const CARD_TAG_X: f32 = 8.0;

/// The install meter runs the width the layout gives it.
pub const METER_H: f32 = 8.0;
/// The lists that fill a step's remaining height stop growing here on a tall
/// screen; a list a screen high is a wall.
pub const LIST_MAX_H: f32 = 520.0;
/// A list narrower than this stops being a list.
pub const COLUMN_MIN_W: f32 = 240.0;

/// The footer bar: back and the one forward action.
pub const FOOTER_H: f32 = 64.0;
/// A footer button's padding.
pub const BUTTON_Y: f32 = 8.0;
pub const BUTTON_X: f32 = 22.0;
/// A form field's inner padding.
pub const INPUT_PAD: f32 = 8.0;

/// A layout card's tile diagram.
pub const GLYPH_H: f32 = 64.0;
/// Air between the tiles of a diagram, so neighbours stay separate shapes.
pub const GLYPH_GAP: f32 = 3.0;

/// The desktop preview on the appearance step.
pub const DESK_H: f32 = 96.0;
/// The bar's strip in the preview.
pub const DESK_BAR_H: f32 = 10.0;
