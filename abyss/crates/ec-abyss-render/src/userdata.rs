// SPDX-License-Identifier: AGPL-3.0-only
//! Per-window and per-layer data the shell writes and the renderer reads.
//!
//! Each lives in smithay's `UserDataMap` on the window or layer surface; the
//! shell (`ec-abyss`) owns the writers and re-exports these types. They sit
//! here, with no `AbyssState` in reach, so the renderer can read them without
//! depending on the shell.

use std::cell::Cell;

use ec_abyss_config::BlurRule;
use smithay::{
    desktop::{LayerMap, LayerSurface as DesktopLayerSurface, Window},
    output::Output,
    utils::{Logical, Point, Rectangle},
};

/// Per-window opacity set by a matched `opacity` rule, read by the renderer.
pub struct RuleOpacity(pub Cell<f32>);

/// Per-window blur override set by a matched `blur` rule, read by the
/// renderer. Picks the window's blur mode; an opaque window still never
/// blurs (see `window_elements`).
pub struct RuleBlur(pub Cell<BlurRule>);

/// The key colours of a tint-keyed strip (ADR 0079): up to four exact RGB
/// colours, each with the opacity its pixels are drawn at. Every other pixel
/// stays opaque.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Keys {
    /// How many of the slots below are live (1..=4).
    pub n: u8,
    pub rgb: [[u8; 3]; 4],
    pub opacity: [f32; 4],
    /// Per key: matched over the whole window and its popups, not just the strip.
    pub everywhere: [bool; 4],
}

impl Keys {
    /// Whether any live key matches everywhere.
    pub fn any_everywhere(&self) -> bool {
        (0..self.n as usize).any(|i| self.everywhere[i])
    }

    /// One key; the constructor the rule path uses.
    pub fn one(rgb: [u8; 3], opacity: f32) -> Self {
        let mut keys = Keys {
            n: 1,
            rgb: [[0; 3]; 4],
            opacity: [1.0; 4],
            everywhere: [false; 4],
        };
        keys.rgb[0] = rgb;
        keys.opacity[0] = opacity;
        keys
    }

    /// The opacity a pixel of colour `px` is drawn at: the first key within
    /// 1/255 per channel, else `None`. The shader does the same test on the GPU.
    pub fn match_pixel(&self, px: [u8; 3]) -> Option<f32> {
        (0..self.n as usize)
            .find(|&i| (0..3).all(|c| px[c].abs_diff(self.rgb[i][c]) <= 1))
            .map(|i| self.opacity[i])
    }
}

/// How a strip is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StripFill {
    /// The whole strip at one alpha (`translucent-region` without a key).
    Fade(f32),
    /// Only the key-coloured pixels translucent.
    Keyed(Keys),
}

/// A translucent region of a window, measured from the top of its geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strip {
    /// Height in logical px; [`Strip::WHOLE`] is the whole window.
    pub top: i32,
    pub fill: StripFill,
}

impl Strip {
    /// `top` for a strip covering the whole window (a client's `body`).
    pub const WHOLE: i32 = i32::MAX;

    pub fn fade(top: i32, alpha: f32) -> Self {
        Strip {
            top,
            fill: StripFill::Fade(alpha),
        }
    }

    pub fn keyed(&self) -> Option<&Keys> {
        match &self.fill {
            StripFill::Keyed(k) => Some(k),
            StripFill::Fade(_) => None,
        }
    }
}

/// `translucent-region`, set by a matched rule. `None` once the rule stops
/// matching, so a reload takes it off again.
pub struct RuleStrip(pub Cell<Option<Strip>>);

/// The same strip, set by a client's add-on helper over the control socket
/// (`set_client_strips`, ADR 0079) instead of by a rule. A user rule wins.
pub struct ClientStrip(pub Cell<Option<Strip>>);

/// The translucent strip pinned on this window: a matched rule's, else the
/// one a client helper reported.
pub fn strip_of(window: &Window) -> Option<Strip> {
    let live = |s: Option<Strip>| s.filter(|s| s.top > 0);
    let ud = window.user_data();
    live(ud.get::<RuleStrip>().and_then(|s| s.0.get()))
        .or_else(|| live(ud.get::<ClientStrip>().and_then(|s| s.0.get())))
}

/// The opacity a matched rule pinned on this window, if any.
pub fn opacity_of(window: &Window) -> Option<f32> {
    window.user_data().get::<RuleOpacity>().map(|o| o.0.get())
}

/// The blur override a matched rule pinned on this window, if any.
pub fn blur_of(window: &Window) -> Option<BlurRule> {
    window.user_data().get::<RuleBlur>().map(|b| b.0.get())
}

/// The tile's inner rect (after the border shrink) a tiled window was last
/// configured to, in global logical coordinates. Render crops tiled windows to
/// it, since some clients (Electron) ignore the configure and overhang their
/// tile (COMP-05, KNOWNBUGS TILE-01). `None` for floating windows.
pub struct TileClip(pub Cell<Option<Rectangle<i32, Logical>>>);

/// The tile rect a tiled window is cropped to, if it is tiled.
pub fn tile_clip(window: &Window) -> Option<Rectangle<i32, Logical>> {
    window.user_data().get::<TileClip>().and_then(|c| c.0.get())
}

/// The output whose workspace laid this window out. Render draws a window on
/// that output only and cuts it to that output's rectangle, so no part of it
/// shows again on a neighbouring monitor sharing the global space.
pub struct OwnerOutput(pub Cell<Option<Output>>);

/// The output that owns `window`, once a layout pass has placed it.
pub fn owner_output(window: &Window) -> Option<Output> {
    let owner = window.user_data().get::<OwnerOutput>()?;
    let out = owner.0.take();
    owner.0.set(out.clone());
    out
}

/// An explicit position for a layer surface, over and above the arrangement
/// `LayerMap::arrange` derives from its anchors and margins.
///
/// The layer map has no notion of a position a layer surface did not ask for,
/// so the delta is kept beside it, in the surface's own user data. Nothing in
/// the protocol sets this; it exists for the harnesses and test hooks that
/// place a layer surface directly (the wlcs `position_window` hook is the only
/// caller today). An unset offset is `(0, 0)`, so every read path below stays
/// exactly the arrangement the layer map produced.
#[derive(Default)]
pub struct LayerOffset(pub Cell<Point<i32, Logical>>);

fn layer_offset(layer: &DesktopLayerSurface) -> Point<i32, Logical> {
    layer
        .user_data()
        .get::<LayerOffset>()
        .map(|o| o.0.get())
        .unwrap_or_default()
}

/// Geometry of a mapped layer surface, including any explicit placement.
///
/// Every hit-test and render path must use this rather than
/// `LayerMap::layer_geometry`, or an explicitly placed layer surface is drawn
/// in one place and clicked in another.
pub fn layer_geometry(map: &LayerMap, layer: &DesktopLayerSurface) -> Option<Rectangle<i32, Logical>> {
    let mut geo = map.layer_geometry(layer)?;
    geo.loc += layer_offset(layer);
    Some(geo)
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn a_pixel_within_one_unit_per_channel_takes_the_keys_opacity() {
        let mut k = Keys::one([0x20, 0x30, 0x40], 0.3);
        k.n = 2;
        k.rgb[1] = [0xff, 0x00, 0x80];
        k.opacity[1] = 0.7;
        assert_eq!(k.match_pixel([0x20, 0x30, 0x40]), Some(0.3));
        assert_eq!(k.match_pixel([0x21, 0x2f, 0x41]), Some(0.3));
        assert_eq!(k.match_pixel([0x22, 0x30, 0x40]), None);
        assert_eq!(k.match_pixel([0xfe, 0x01, 0x7f]), Some(0.7));
        // Text, icons: anything else stays opaque.
        assert_eq!(k.match_pixel([0xff, 0xff, 0xff]), None);
        assert!(!k.any_everywhere());
        k.everywhere[1] = true;
        assert!(k.any_everywhere());
        // A dead slot never matches.
        assert_eq!(k.match_pixel([0, 0, 0]), None);
    }
}
