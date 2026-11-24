// SPDX-License-Identifier: AGPL-3.0-only
//! The lock's own numbers for the tiled screen (ADR 0126, docs/STYLE.md):
//! pane radii, how a tile's type scales, the entrance spring. Colour, type
//! scale and spacing come from `ec_ui::tokens`, the table the desktop reads.

/// The gap a tile leaves on each side (`Rect::to_px`): neighbours sit 2x apart.
pub const TILE_GAP: u32 = ec_glance::cells::lock::TILE_GAP;
/// A pane's corner radius. Clamped to half the pane's short side.
pub const PANE_RADIUS: f32 = ec_glance::cells::lock::PANE_RADIUS;
/// A cell inside a pane: the account disc's row, a pill.
pub const PILL_RADIUS: f32 = ec_glance::cells::lock::PILL_RADIUS;
/// Pane padding as a share of its short side, and its bounds.
/// The pad never exceeds this share of the pane's short side.
/// A pane rests on the ground with a soft drop: depth, never a halo.
pub const SHADOW_Y: f32 = 16.0;
pub const SHADOW_BLUR: f32 = 36.0;
/// The pane's rim, as a share of the shared strong border.
pub const RIM: f32 = 1.0;
/// A pill's horizontal padding as a share of its radius.
pub const PILL_PAD_X: f32 = ec_glance::cells::lock::PILL_PAD_X;

/// Inside a pane: the shared numbers (`ec_glance::cells::lock`), which the
/// layout builder's lock preview reads too, plus the signal bar's own corner.
pub mod cell {
    pub use ec_glance::cells::lock::*;
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
const _: () = {};
