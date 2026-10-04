// SPDX-License-Identifier: AGPL-3.0-only
//! `layer-open` and `layer-close` for every layer-shell surface (COMP-02 §9).
//!
//! Compositor-side, so a third-party bar, notification daemon or launcher
//! animates exactly like the bundled panes. Render-only like everything in
//! this module: `LayerMap::arrange`, exclusive zones and hit-testing use the
//! target geometry from the first frame, and the surface is only *drawn*
//! displaced. Close plays out a [`Ghost::Layer`] cloned from the surface's last
//! textures, so it outlives the buffer and the client.
//!
//! Never animated: a surface that covers the whole output (a scrim, a
//! screenshot selector, anything an `Overlay` client uses to take the screen).
//! The session lock and the trusted UI are not layer-shell surfaces and never
//! reach this code.

use std::time::Instant;

use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::{layer_map_for_output, LayerSurface};
use smithay::output::Output;
use smithay::utils::{Logical, Size};
use smithay::wayland::shell::wlr_layer::Anchor;

use ec_abyss_config::animations::Event;
use ec_abyss_config::Animations;

use super::track::{Channels, Track};
use super::{resolve, AnimStore};
use crate::userdata;

/// Whether a surface of `size` covers the whole of an output of `output`.
pub fn covers_output(size: Size<i32, Logical>, output: Size<i32, Logical>) -> bool {
    size.w >= output.w && size.h >= output.h
}

const FADE: Channels = Channels {
    alpha: -1.0,
    ..Channels::ZERO
};

/// `pop`: 0.92 → 1 with a fade.
const POP: Channels = Channels {
    sx: -0.08,
    sy: -0.08,
    ..FADE
};

/// The side a layer surface slides in from, as the displacement of its start
/// from its target, or `None` when its anchors name no edge to come from:
/// anchored to no edge, to all four, or only to an opposite pair (centred on
/// the other axis). A corner takes its horizontal edge.
pub fn slide_from(anchor: Anchor, size: Size<i32, Logical>) -> Option<Channels> {
    let net = |hi: Anchor, lo: Anchor| anchor.contains(hi) as i32 - anchor.contains(lo) as i32;
    let (x, y) = (net(Anchor::RIGHT, Anchor::LEFT), net(Anchor::BOTTOM, Anchor::TOP));
    if x != 0 {
        Some(Channels {
            dx: (x * size.w) as f64,
            ..FADE
        })
    } else if y != 0 {
        Some(Channels {
            dy: (y * size.h) as f64,
            ..FADE
        })
    } else {
        None
    }
}

/// How `style` displaces a layer surface of `size` and `anchor` at the start of
/// opening (and the end of closing), or `None` when it is not animated at all
/// because it covers an `output`-sized output. `slide` on a surface with no
/// edge to come from is a `pop`.
pub fn layer_motion(
    style: &str,
    anchor: Anchor,
    size: Size<i32, Logical>,
    output: Size<i32, Logical>,
) -> Option<Channels> {
    if covers_output(size, output) {
        return None;
    }
    Some(match style {
        "fade" => FADE,
        "slide" => slide_from(anchor, size).unwrap_or(POP),
        // "pop", and the fallback.
        _ => POP,
    })
}

impl<W> AnimStore<W> {
    /// `layer-open`: start a track for each layer surface of `output` that has
    /// just mapped, that is, has its first buffer. `output_size` is the
    /// output's logical size.
    ///
    /// Run once per output per frame after `sync`, which owns the clock and
    /// retires finished tracks. A surface already seen draws as it always did;
    /// one that drops its buffer is forgotten, so mapping again opens again.
    pub fn sync_layers(&mut self, output: &Output, anims: &Animations, output_size: Size<i32, Logical>) {
        let (now, shed) = (self.now, self.shed);
        let map = layer_map_for_output(output);
        self.layers
            .retain(|(o, l), _| o != output || map.layers().any(|m| m == l));
        let mut open: Option<Option<_>> = None;
        for layer in map.layers() {
            let key = (output.clone(), layer.clone());
            let mapped =
                with_renderer_surface_state(layer.wl_surface(), |s| s.buffer().is_some()).unwrap_or(false);
            if !mapped {
                self.layers.remove(&key);
                continue;
            }
            if self.layers.contains_key(&key) {
                continue;
            }
            let track = userdata::layer_geometry(&map, layer).and_then(|geo| {
                let (leg, style) =
                    (*open.get_or_insert_with(|| resolve(anims, Event::LayerOpen, now, shed)))?;
                let from = layer_motion(style, layer.cached_state().anchor, geo.size, output_size)?;
                Some(Track::arrive(leg, from))
            });
            self.running |= track.is_some();
            self.layers.insert(key, track);
        }
    }

    /// How to draw `layer` on `output` this frame; the identity unless it is
    /// opening.
    pub fn layer_transform(&self, output: &Output, layer: &LayerSurface) -> super::Transform {
        match self.layers.get(&(output.clone(), layer.clone())) {
            Some(Some(track)) => super::Transform::identity().compose(track.at(self.now).0),
            _ => super::Transform::identity(),
        }
    }

    /// Drop finished layer tracks. Called by `sync`, for every output at once,
    /// so a powered-off output cannot hold the repaint loop open.
    pub(super) fn retire_layers(&mut self, now: Instant) {
        for track in self.layers.values_mut() {
            if track.is_some_and(|t| t.done(now)) {
                *track = None;
            }
        }
    }

    pub(super) fn layers_running(&self) -> bool {
        self.layers.values().any(Option::is_some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out() -> Size<i32, Logical> {
        Size::from((1920, 1080))
    }

    fn a(flags: &[Anchor]) -> Anchor {
        flags.iter().fold(Anchor::empty(), |acc, f| acc | *f)
    }

    #[test]
    fn slide_comes_in_from_the_anchored_edge() {
        let bar = Size::from((1920, 40));
        let side = Size::from((300, 1080));
        let cases = [
            // A bar on the top edge, stretched across: rises from above.
            (a(&[Anchor::TOP, Anchor::LEFT, Anchor::RIGHT]), bar, (0.0, -40.0)),
            (
                a(&[Anchor::BOTTOM, Anchor::LEFT, Anchor::RIGHT]),
                bar,
                (0.0, 40.0),
            ),
            (a(&[Anchor::TOP]), Size::from((400, 60)), (0.0, -60.0)),
            (
                a(&[Anchor::LEFT, Anchor::TOP, Anchor::BOTTOM]),
                side,
                (-300.0, 0.0),
            ),
            (
                a(&[Anchor::RIGHT, Anchor::TOP, Anchor::BOTTOM]),
                side,
                (300.0, 0.0),
            ),
            // A corner (a toast stack) takes its horizontal edge.
            (
                a(&[Anchor::TOP, Anchor::RIGHT]),
                Size::from((360, 120)),
                (360.0, 0.0),
            ),
            (
                a(&[Anchor::BOTTOM, Anchor::LEFT]),
                Size::from((360, 120)),
                (-360.0, 0.0),
            ),
        ];
        for (anchor, size, (dx, dy)) in cases {
            let c = slide_from(anchor, size).unwrap_or_else(|| panic!("{anchor:?} has an edge"));
            assert_eq!((c.dx, c.dy), (dx, dy), "{anchor:?}");
            // It fades as it travels, and does not scale.
            assert_eq!((c.sx, c.sy, c.alpha), (0.0, 0.0, -1.0));
        }
    }

    #[test]
    fn no_edge_to_come_from_is_a_pop() {
        let size = Size::from((600, 400));
        for anchor in [
            Anchor::empty(),
            Anchor::all(),
            a(&[Anchor::LEFT, Anchor::RIGHT]),
            a(&[Anchor::TOP, Anchor::BOTTOM]),
        ] {
            assert_eq!(slide_from(anchor, size), None, "{anchor:?}");
            assert_eq!(
                layer_motion("slide", anchor, size, out()),
                Some(POP),
                "{anchor:?}"
            );
        }
    }

    #[test]
    fn styles_map_to_their_channels() {
        let anchor = a(&[Anchor::TOP, Anchor::LEFT, Anchor::RIGHT]);
        let size = Size::from((1920, 40));
        assert_eq!(layer_motion("fade", anchor, size, out()), Some(FADE));
        assert_eq!(layer_motion("pop", anchor, size, out()), Some(POP));
        let slide = layer_motion("slide", anchor, size, out()).unwrap();
        assert_eq!((slide.dx, slide.dy), (0.0, -40.0));
    }

    #[test]
    fn a_full_output_layer_is_never_animated() {
        let all = Anchor::all();
        for style in ["slide", "fade", "pop", "none", "pack:style"] {
            // A scrim, and one an output-sized client took without anchors.
            assert_eq!(layer_motion(style, all, out(), out()), None, "{style}");
            assert_eq!(
                layer_motion(style, Anchor::empty(), out(), out()),
                None,
                "{style}"
            );
            // Bigger than the output (hanging off it) still covers it.
            assert_eq!(
                layer_motion(style, all, Size::from((2000, 1100)), out()),
                None,
                "{style}"
            );
        }
        // One pixel short on either axis is a sheet, and animates.
        assert!(layer_motion("fade", all, Size::from((1919, 1080)), out()).is_some());
        assert!(layer_motion("fade", all, Size::from((1920, 1079)), out()).is_some());
        // A full-width bar is not a scrim.
        assert!(layer_motion(
            "slide",
            a(&[Anchor::TOP, Anchor::LEFT, Anchor::RIGHT]),
            Size::from((1920, 40)),
            out()
        )
        .is_some());
    }
}
