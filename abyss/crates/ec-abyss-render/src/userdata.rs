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
