// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse_glass_v1` pane state and look (COMP-02 §9).
//!
//! A DE client hands the compositor a list of rectangles ("panes") on one of
//! its surfaces; the compositor draws real glass under each. This module is
//! the state-free half: the double-buffered pane list as smithay
//! [`Cacheable`] surface state (the protocol handler writes `pending`, the
//! commit latches `current`, the renderer reads `current` straight from the
//! surface), the wire parser, and the pure maps from a pane to what the glass
//! program is told (rim strength, fill lift, bend band).
//!
//! No allocation: the list is a fixed `[Pane; 64]` plus a length.

use smithay::{
    reexports::wayland_server::DisplayHandle,
    utils::{Logical, Physical, Rectangle},
    wayland::compositor::Cacheable,
};

/// Most panes one `set_panes` may carry.
pub const MAX_PANES: usize = 64;
/// One packed record: nine native-endian 32-bit fields.
pub const RECORD_BYTES: usize = 36;
/// Band width (logical px) of a pane whose bezel is 0.
pub const DEFAULT_BEZEL: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Plain,
    Focused,
    Away,
    Latent,
}

/// `state` bits.
pub const HOVERED: u32 = 1;
pub const PRESSED: u32 = 2;

/// One pane, surface-local logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pane {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub radius: u32,
    pub bezel: u32,
    pub tone: Tone,
    pub state: u32,
    pub alpha: u8,
}

/// Why a `set_panes` array was refused (`bad_pane`, `too_many_panes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Not a multiple of 36 bytes, an unknown tone, a negative size, or an
    /// alpha above 255.
    BadPane,
    TooManyPanes,
}

/// A fixed-capacity pane list.
#[derive(Debug, Clone)]
pub struct PaneSet {
    panes: [Pane; MAX_PANES],
    len: usize,
}

impl Default for PaneSet {
    fn default() -> Self {
        Self {
            panes: [Pane::default(); MAX_PANES],
            len: 0,
        }
    }
}

impl PartialEq for PaneSet {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl PaneSet {
    pub fn as_slice(&self) -> &[Pane] {
        &self.panes[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bounds of every pane, surface-local logical, clipped to a surface
    /// of `size`; `None` if nothing is left. This, not the pane list, is what
    /// the backdrop is cached on: a chip that grows inside the same bounds
    /// does not re-blur.
    pub fn union(&self, size: smithay::utils::Size<i32, Logical>) -> Option<Rectangle<i32, Logical>> {
        let surface = Rectangle::from_size(size);
        self.as_slice()
            .iter()
            .filter_map(|p| {
                Rectangle::<i32, Logical>::new((p.x, p.y).into(), (p.w, p.h).into()).intersection(surface)
            })
            .filter(|r| !r.is_empty())
            .reduce(|a, b| a.merge(b))
    }
}

/// Parse a `set_panes` array.
pub fn parse_panes(bytes: &[u8]) -> Result<PaneSet, ParseError> {
    if bytes.len() % RECORD_BYTES != 0 {
        return Err(ParseError::BadPane);
    }
    let count = bytes.len() / RECORD_BYTES;
    if count > MAX_PANES {
        return Err(ParseError::TooManyPanes);
    }
    let mut set = PaneSet::default();
    for (slot, rec) in set.panes.iter_mut().zip(bytes.chunks_exact(RECORD_BYTES)) {
        let word = |i: usize| -> [u8; 4] { [rec[i * 4], rec[i * 4 + 1], rec[i * 4 + 2], rec[i * 4 + 3]] };
        let (x, y, w, h) = (
            i32::from_ne_bytes(word(0)),
            i32::from_ne_bytes(word(1)),
            i32::from_ne_bytes(word(2)),
            i32::from_ne_bytes(word(3)),
        );
        let tone = match u32::from_ne_bytes(word(6)) {
            0 => Tone::Plain,
            1 => Tone::Focused,
            2 => Tone::Away,
            3 => Tone::Latent,
            _ => return Err(ParseError::BadPane),
        };
        let alpha = u32::from_ne_bytes(word(8));
        if w < 0 || h < 0 || alpha > 255 {
            return Err(ParseError::BadPane);
        }
        *slot = Pane {
            x,
            y,
            w,
            h,
            radius: u32::from_ne_bytes(word(4)),
            bezel: u32::from_ne_bytes(word(5)),
            tone,
            state: u32::from_ne_bytes(word(7)) & (HOVERED | PRESSED),
            alpha: alpha as u8,
        };
    }
    set.len = count;
    Ok(set)
}

/// A surface's `eclipse_glass_v1` panes, double-buffered. `None` is "no
/// object": the surface keeps whatever glass it would have had.
#[derive(Debug, Clone, Default)]
pub struct GlassState {
    pub panes: Option<PaneSet>,
}

impl Cacheable for GlassState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        self.clone()
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Rim strength relative to a plain cell at rest, from the DE tokens
/// (`CELL_RIM` .016, hover .026, away .006, focus .040, focus hover .055).
const RIM_PLAIN: f32 = 1.0;
const RIM_PLAIN_HOVER: f32 = 1.625;
const RIM_AWAY: f32 = 0.375;
const RIM_FOCUS: f32 = 2.5;
const RIM_FOCUS_HOVER: f32 = 3.4375;

/// What the glass program is told for one pane: the rim multiplier and the
/// fill lift (a small mix toward white). No colour: gold never enters glass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub rim: f32,
    pub lift: f32,
}

/// The look of `pane`, or `None` when it draws nothing (latent at rest, or
/// fully transparent). Everything scales by `alpha / 255`.
pub fn look(pane: &Pane) -> Option<Look> {
    let pressed = pane.state & PRESSED != 0;
    let hot = pressed || pane.state & HOVERED != 0;
    let (rim, lift) = match (pane.tone, hot) {
        (Tone::Latent, false) => return None,
        (Tone::Plain, false) => (RIM_PLAIN, 0.0),
        (Tone::Plain | Tone::Latent, true) => (RIM_PLAIN_HOVER, if pressed { 0.02 } else { 0.012 }),
        (Tone::Away, false) => (RIM_AWAY, 0.0),
        (Tone::Away, true) => (RIM_PLAIN_HOVER * RIM_AWAY, if pressed { 0.02 } else { 0.012 }),
        (Tone::Focused, false) => (RIM_FOCUS, 0.014),
        (Tone::Focused, true) => (RIM_FOCUS_HOVER, if pressed { 0.028 } else { 0.02 }),
    };
    if pane.alpha == 0 {
        return None;
    }
    let a = pane.alpha as f32 / 255.0;
    Some(Look {
        rim: rim * a,
        lift: lift * a,
    })
}

/// The pane's box in output-local physical pixels, clipped to the surface
/// (`size`, logical) and to `crop` if given. `loc` is the surface origin,
/// output-local physical; `scale` the output scale.
pub fn pane_rect(
    pane: &Pane,
    size: smithay::utils::Size<i32, Logical>,
    loc: smithay::utils::Point<i32, Physical>,
    scale: smithay::utils::Scale<f64>,
) -> Option<Rectangle<i32, Physical>> {
    let logical = Rectangle::<i32, Logical>::new((pane.x, pane.y).into(), (pane.w, pane.h).into())
        .intersection(Rectangle::from_size(size))?;
    let r = Rectangle::<i32, Physical>::new(
        (
            loc.x + (logical.loc.x as f64 * scale.x).round() as i32,
            loc.y + (logical.loc.y as f64 * scale.y).round() as i32,
        )
            .into(),
        (
            (logical.size.w as f64 * scale.x).round() as i32,
            (logical.size.h as f64 * scale.y).round() as i32,
        )
            .into(),
    );
    (!r.is_empty()).then_some(r)
}

/// The pane's bend band in physical px: its bezel (or the default), at most
/// half its shorter side.
pub fn band(pane: &Pane, rect: Rectangle<i32, Physical>, upscale: f64) -> f32 {
    let logical = if pane.bezel == 0 {
        DEFAULT_BEZEL
    } else {
        pane.bezel
    };
    let px = (logical as f64 * upscale) as f32;
    px.min(rect.size.w.min(rect.size.h) as f32 * 0.5).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(f: [u32; 9]) -> Vec<u8> {
        f.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    fn rec(x: i32, y: i32, w: i32, h: i32, tone: u32, state: u32, alpha: u32) -> Vec<u8> {
        record([x as u32, y as u32, w as u32, h as u32, 8, 0, tone, state, alpha])
    }

    #[test]
    fn parses_records_in_order() {
        let mut b = rec(1, 2, 30, 20, 0, 0, 255);
        b.extend(rec(40, 2, 30, 20, 1, 1, 128));
        let set = parse_panes(&b).unwrap();
        assert_eq!(set.len(), 2);
        let p = set.as_slice();
        assert_eq!((p[0].x, p[0].y, p[0].w, p[0].h, p[0].radius), (1, 2, 30, 20, 8));
        assert_eq!(p[1].tone, Tone::Focused);
        assert_eq!(p[1].state, HOVERED);
        assert_eq!(p[1].alpha, 128);
    }

    #[test]
    fn empty_array_is_no_panes() {
        assert!(parse_panes(&[]).unwrap().is_empty());
    }

    #[test]
    fn bad_records_are_bad_pane() {
        assert!(matches!(parse_panes(&[0u8; 35]), Err(ParseError::BadPane)));
        assert!(matches!(parse_panes(&[0u8; 37]), Err(ParseError::BadPane)));
        assert!(matches!(
            parse_panes(&rec(0, 0, 1, 1, 4, 0, 0)),
            Err(ParseError::BadPane)
        ));
        assert!(matches!(
            parse_panes(&rec(0, 0, -1, 1, 0, 0, 0)),
            Err(ParseError::BadPane)
        ));
        assert!(matches!(
            parse_panes(&rec(0, 0, 1, -1, 0, 0, 0)),
            Err(ParseError::BadPane)
        ));
        assert!(matches!(
            parse_panes(&rec(0, 0, 1, 1, 0, 0, 256)),
            Err(ParseError::BadPane)
        ));
    }

    #[test]
    fn the_cap_is_64() {
        let one = rec(0, 0, 4, 4, 0, 0, 255);
        let ok: Vec<u8> = one.iter().copied().cycle().take(RECORD_BYTES * 64).collect();
        assert_eq!(parse_panes(&ok).unwrap().len(), 64);
        let over: Vec<u8> = one.iter().copied().cycle().take(RECORD_BYTES * 65).collect();
        assert!(matches!(parse_panes(&over), Err(ParseError::TooManyPanes)));
    }

    #[test]
    fn the_union_ignores_inner_changes() {
        let size = (200, 40).into();
        let a =
            parse_panes(&[rec(10, 4, 30, 20, 0, 0, 255), rec(100, 4, 50, 20, 0, 0, 255)].concat()).unwrap();
        // The inner chip grew and a pane reordered, inside the same bounds.
        let b =
            parse_panes(&[rec(10, 4, 60, 20, 1, 1, 200), rec(100, 4, 50, 20, 0, 0, 255)].concat()).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.union(size), b.union(size));
        let u = a.union(size).unwrap();
        assert_eq!((u.loc.x, u.loc.y, u.size.w, u.size.h), (10, 4, 140, 20));
    }

    #[test]
    fn the_union_is_clipped_to_the_surface() {
        let set = parse_panes(&rec(-10, -10, 50, 30, 0, 0, 255)).unwrap();
        let u = set.union((100, 100).into()).unwrap();
        assert_eq!((u.loc.x, u.loc.y, u.size.w, u.size.h), (0, 0, 40, 20));
        assert!(parse_panes(&rec(500, 500, 5, 5, 0, 0, 255))
            .unwrap()
            .union((100, 100).into())
            .is_none());
    }

    fn pane(tone: Tone, state: u32, alpha: u8) -> Pane {
        Pane {
            tone,
            state,
            alpha,
            w: 10,
            h: 10,
            ..Pane::default()
        }
    }

    #[test]
    fn tone_and_state_map_to_rim_and_lift() {
        let plain = look(&pane(Tone::Plain, 0, 255)).unwrap();
        let focus = look(&pane(Tone::Focused, 0, 255)).unwrap();
        let away = look(&pane(Tone::Away, 0, 255)).unwrap();
        let hover = look(&pane(Tone::Plain, HOVERED, 255)).unwrap();
        let press = look(&pane(Tone::Plain, PRESSED, 255)).unwrap();
        assert!(focus.rim > hover.rim && hover.rim > plain.rim && plain.rim > away.rim);
        assert_eq!(away.lift, 0.0);
        assert_eq!(plain.lift, 0.0);
        assert!(press.lift > hover.lift && hover.lift > 0.0);
        assert!((focus.rim / plain.rim - 2.5).abs() < 1e-6);
        assert!(look(&pane(Tone::Latent, 0, 255)).is_none());
        assert_eq!(look(&pane(Tone::Latent, HOVERED, 255)), Some(hover));
    }

    #[test]
    fn alpha_scales_everything() {
        let full = look(&pane(Tone::Focused, HOVERED, 255)).unwrap();
        let half = look(&pane(Tone::Focused, HOVERED, 51)).unwrap();
        assert!((half.rim - full.rim * 0.2).abs() < 1e-5);
        assert!((half.lift - full.lift * 0.2).abs() < 1e-6);
        assert!(look(&pane(Tone::Plain, 0, 0)).is_none());
    }

    #[test]
    fn rects_are_physical_and_clipped() {
        let p = Pane {
            x: 10,
            y: 4,
            w: 300,
            h: 20,
            ..Pane::default()
        };
        let r = pane_rect(
            &p,
            (100, 40).into(),
            (1000, 0).into(),
            smithay::utils::Scale::from(2.0),
        )
        .unwrap();
        assert_eq!((r.loc.x, r.loc.y, r.size.w, r.size.h), (1020, 8, 180, 40));
        assert_eq!(band(&p, r, 2.0), 12.0);
    }
}
