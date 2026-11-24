// SPDX-License-Identifier: AGPL-3.0-only
//! The lock's own numbers for the tiled screen (ADR 0126, docs/STYLE.md):
//! pane radii, how a tile's type scales, the entrance spring. Colour, type
//! scale and spacing come from `ec_ui::tokens`, the table the desktop reads.

/// The gap a tile leaves on each side (`Rect::to_px`): neighbours sit 2x apart.
pub const TILE_GAP: u32 = 8;
/// A pane's corner radius. Clamped to half the pane's short side.
pub const PANE_RADIUS: f32 = 22.0;
/// A cell inside a pane: the account disc's row, a pill.
pub const PILL_RADIUS: f32 = 999.0;
/// Pane padding as a share of its short side, and its bounds.
/// The pad never exceeds this share of the pane's short side.
/// A pane rests on the ground with a soft drop: depth, never a halo.
pub const SHADOW_Y: f32 = 16.0;
pub const SHADOW_BLUR: f32 = 36.0;
/// The pane's rim, as a share of the shared strong border.
pub const RIM: f32 = 1.0;
/// A pill's horizontal padding as a share of its radius.
pub const PILL_PAD_X: f32 = 0.7;

/// Inside a pane.
pub mod cell {
    pub const FIELD_MIN: f32 = 44.0;
    /// Below this the Unlock pill trims its padding to stay inside the field.
    pub const FIELD_ROOMY: f32 = 52.0;
    pub const UNLOCK_PAD_TIGHT: f32 = 2.0;
    pub const FIELD_MAX: f32 = 56.0;
    /// The line under the field: what is happening.
    pub const NOTE_H: f32 = 18.0;
    pub const NOTE_GAP: f32 = 6.0;
    /// The hint line under the note, when the pane has the room.
    pub const HINT_H: f32 = 22.0;
    /// The account disc in the greeting pill: a share of the pill, clamped.
    pub const AVATAR_FRAC: f32 = 0.9;
    pub const AVATAR_MIN: f32 = 22.0;
    pub const AVATAR_MAX: f32 = 44.0;
    /// The initial's size as a share of its disc.
    pub const INITIAL_FRAC: f32 = 0.5;
    /// The greeting drops its state word below this inner width.
    pub const GREETING_WORD_W: f32 = 220.0;
    pub const BAR_RADIUS: f32 = 2.0;
}

/// Motion. A pane springs up and fades in when the screen appears: a damped
/// spring with about a tenth overshoot, a bubble and not a slide.
pub mod motion {
    /// Spring stiffness (1/s^2) and damping (1/s).
    pub const STIFFNESS: f32 = 300.0;
    pub const DAMPING: f32 = 20.0;
    /// The delay between tiles on appearance, ms, in layout order.
    pub const STAGGER_MS: u64 = 70;
    /// How far a tile rises as it appears, px.
    pub const RISE: f32 = 28.0;
    /// After this long a spring is at rest to the eye, ms.
    pub const SETTLE_MS: u64 = 900;
}

/// The glance feed: how long one sysfs read may take, and how often to poll.
pub mod glance {
    use std::time::Duration;
    pub const LIMIT: Duration = Duration::from_millis(900);
    pub const EVERY: Duration = Duration::from_secs(5);
}

// Every clamp in the view takes its bounds in order; a reversed pair would
// panic at run time, so the order is checked at compile time.
const _: () = {
    assert!(cell::FIELD_MIN < cell::FIELD_MAX);
    assert!(cell::AVATAR_MIN < cell::AVATAR_MAX);
};
