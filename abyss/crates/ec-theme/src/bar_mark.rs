// SPDX-License-Identifier: AGPL-3.0-only
//! The bar's launcher mark, as far as the compositor needs it to draw the
//! capture eye over the same pixels (state-free, no deps).
//!
//! Every value equals its `ec_ui::tokens::bar` counterpart; a test in
//! `ec-ui` (`bar_mark_matches_tokens`) fails if either side moves alone.

/// `bar::PILL_H`: the pill's height.
pub const PILL_H: f32 = 40.0;
/// `bar::EDGE`: the row's inset from the pill's left.
pub const EDGE: f32 = 8.0;
/// `bar::TASK_MIN`: the launcher button's width.
pub const TASK_MIN: f32 = 34.0;
/// `bar::TASK_H`: the launcher button's height.
pub const TASK_H: f32 = 34.0;
/// `bar::RING`: thickness of the resting ring.
pub const RING: f32 = 2.0;
/// `bar::EYE_DISC` (`size::ICON * 0.85`): the mark's outer diameter.
pub const EYE_DISC: f32 = 20.0 * 0.85;
/// `bar::EYE_PUPIL`: pupil radius, fraction of the iris radius.
pub const EYE_PUPIL: f32 = 0.4;
/// `bar::EYE_WANDER`: how far the pupil's centre may stray, fraction of the iris radius.
pub const EYE_WANDER: f32 = 0.27;
/// `bar::EYE_GLANCE`: a glance lands at least this fraction of the wander off centre.
pub const EYE_GLANCE: f32 = 0.8;
/// `bar::EYE_RECENTRE`: one dart in this many looks straight ahead.
pub const EYE_RECENTRE: u64 = 6;
/// `bar::EYE_RIM`: the rim's thickness.
pub const EYE_RIM: f32 = RING;
/// `bar::EYE_BLEED`: how far past the disc the eye draws its edge.
pub const EYE_BLEED: f32 = 0.75;
/// `bar::EYE_DART_MS`: one saccade.
pub const EYE_DART_MS: u64 = 120;
/// `bar::EYE_RESIZE_MS`: the pupil opening.
pub const EYE_RESIZE_MS: u64 = 200;
/// `bar::EYE_HOLD_MIN_MS`: shortest hold between darts.
pub const EYE_HOLD_MIN_MS: u64 = 400;
/// `bar::EYE_HOLD_MAX_MS`: longest hold between darts.
pub const EYE_HOLD_MAX_MS: u64 = 1400;
/// `bar::EYE_FRAME_MS`: frame interval while moving.
pub const EYE_FRAME_MS: u64 = 16;
