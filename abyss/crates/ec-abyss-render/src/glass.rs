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

use ec_protocols::glass::{JoinRecord, JOIN_RECORD_BYTES};
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
/// The pane is cut to the set's clip rectangle (protocol version 2).
pub const CLIPPED: u32 = 4;

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

/// Most joins one `set_joins` may carry (`too_many_joins`).
pub const MAX_JOINS: usize = 64;
/// Most panes one joined shape may hold; the rest draw alone (ADR 0130).
pub const MAX_SHAPE_PANES: usize = 16;
/// Largest fillet radius a join may ask for, logical px.
pub const MAX_JOIN_RADIUS: u32 = 64;

/// One join of `set_joins` (ADR 0130), checked against the pane list it was
/// latched with: `a` and `b` are indices into it, `a != b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Join {
    pub a: u8,
    pub b: u8,
    /// Fillet radius where the two panes meet, logical px, at most
    /// [`MAX_JOIN_RADIUS`].
    pub radius: u32,
}

/// Why a `set_joins` array was refused (`bad_join`, `too_many_joins`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinError {
    /// Not a multiple of 12 bytes, an index at or past the pane count, or a
    /// pane joined to itself.
    BadJoin,
    TooManyJoins,
}

/// A fixed-capacity join list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinSet {
    joins: [Join; MAX_JOINS],
    len: usize,
}

impl Default for JoinSet {
    fn default() -> Self {
        Self {
            joins: [Join::default(); MAX_JOINS],
            len: 0,
        }
    }
}

impl JoinSet {
    pub fn as_slice(&self) -> &[Join] {
        &self.joins[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Parse a `set_joins` array against a pane list of `panes` entries. An empty
/// array clears the joins. The radius is capped at [`MAX_JOIN_RADIUS`].
pub fn parse_joins(bytes: &[u8], panes: usize) -> Result<JoinSet, JoinError> {
    if bytes.len() % JOIN_RECORD_BYTES != 0 {
        return Err(JoinError::BadJoin);
    }
    let count = bytes.len() / JOIN_RECORD_BYTES;
    if count > MAX_JOINS {
        return Err(JoinError::TooManyJoins);
    }
    let mut set = JoinSet::default();
    for (slot, rec) in set.joins.iter_mut().zip(bytes.chunks_exact(JOIN_RECORD_BYTES)) {
        let rec = JoinRecord::from_bytes(rec).ok_or(JoinError::BadJoin)?;
        let index = |i: u32| usize::try_from(i).ok().filter(|&i| i < panes);
        let (Some(a), Some(b)) = (index(rec.a), index(rec.b)) else {
            return Err(JoinError::BadJoin);
        };
        if a == b {
            return Err(JoinError::BadJoin);
        }
        // Below MAX_PANES, so both indices fit a byte.
        *slot = Join {
            a: a as u8,
            b: b as u8,
            radius: rec.radius.min(MAX_JOIN_RADIUS),
        };
    }
    set.len = count;
    Ok(set)
}

/// A fixed-capacity pane list.
#[derive(Debug, Clone)]
pub struct PaneSet {
    panes: [Pane; MAX_PANES],
    len: usize,
    /// `set_clip` (ADR 0127): the rectangle `CLIPPED` panes are cut to,
    /// surface-local logical. `None` clears it.
    pub clip: Option<Rectangle<i32, Logical>>,
    /// `set_joins` (ADR 0130), latched with the panes. Empty after any
    /// `set_panes`, since the indices refer to the list it replaces.
    joins: JoinSet,
}

impl Default for PaneSet {
    fn default() -> Self {
        Self {
            panes: [Pane::default(); MAX_PANES],
            len: 0,
            clip: None,
            joins: JoinSet::default(),
        }
    }
}

impl PartialEq for PaneSet {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice() && self.clip == other.clip && self.joins == other.joins
    }
}

impl PaneSet {
    pub fn as_slice(&self) -> &[Pane] {
        &self.panes[..self.len]
    }

    /// The latched joins (ADR 0130), in array order.
    pub fn joins(&self) -> &[Join] {
        self.joins.as_slice()
    }

    /// Replace the joins; the caller has checked them against this list
    /// ([`parse_joins`]).
    pub fn set_joins(&mut self, joins: JoinSet) {
        self.joins = joins;
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

/// `set_clip`: a zero width or height clears the clip, a negative one is
/// `bad_pane` (`None`), anything else is the clip rectangle.
pub fn parse_clip(x: i32, y: i32, w: i32, h: i32) -> Option<Option<Rectangle<i32, Logical>>> {
    if w < 0 || h < 0 {
        return None;
    }
    Some((w > 0 && h > 0).then(|| Rectangle::new((x, y).into(), (w, h).into())))
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
            state: u32::from_ne_bytes(word(7)) & (HOVERED | PRESSED | CLIPPED),
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

/// The light scheme's pane look (ADR 0084, STYLE.md Light, the `LIGHT` cell
/// table in ec-theme). Each depth step moves toward the ink: a rest fill of
/// ink .05, hover .09, press .13 (`shade`), and an ink hairline of .12 at rest,
/// .18 hovered or focused, .24 focused and hovered. A minimized (`Away`) chip
/// has no fill. The focused pane takes no ink: it is the one lift, with the
/// white `lift` and the crisp ink rim.
///
/// `rim` is scaled so that at the default `glass.rim` (0.5) the base lobe of
/// the shader's hairline (`LINE_BASE` .05, weight `lobe * rim_color.a * 2 *
/// rim`) reads these alphas: `rim = alpha / 0.05`, which is `alpha * 20`.
const LIGHT_REST: f32 = 0.05;
const LIGHT_HOVER: f32 = 0.09;
const LIGHT_PRESS: f32 = 0.13;
const LIGHT_RIM_REST: f32 = 0.12 * 20.0;
const LIGHT_RIM_HOT: f32 = 0.18 * 20.0;
const LIGHT_RIM_FOCUS: f32 = 0.18 * 20.0;
const LIGHT_RIM_FOCUS_HOT: f32 = 0.24 * 20.0;
const LIGHT_RIM_AWAY: f32 = 0.05 * 20.0;

/// The ink (`rgba(26,22,16)`, premultiplied at alpha 1) a light pane's hairline
/// is drawn in. Dark panes keep the white one (`rim_color` opaque white).
pub const LIGHT_RIM_INK: [f32; 4] = [0.102, 0.086, 0.063, 1.0];

/// What the glass program is told for one pane: the rim multiplier, the white
/// fill lift (a small mix toward white), and the light scheme's ink fill
/// (`shade`, a mix toward ink; always 0 on dark). No colour: gold never enters
/// glass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub rim: f32,
    pub lift: f32,
    pub shade: f32,
    /// Weight of the glass smoke and adaptive tint; 1.0 except when stacked.
    pub smoke: f32,
}

/// Stacked glass: a pane whose backdrop already holds a window's own smoked
/// glass (a sheet over a glass window, a chip on its bezel). Smoking it again
/// at full weight darkens two layers into one slab, and the text under it
/// disappears. So it keeps the blur, takes a fraction of the smoke and
/// adaptive tint, and reads as raised by a small white lift, a firmer rim
/// and the contact shadow instead of by being darker.
pub const STACKED_SMOKE: f32 = 0.45;
/// Light glass is a veil already; it thins further so the frosted text under
/// the sheet stays readable against the window's own whitening.
pub const STACKED_SMOKE_LIGHT: f32 = 0.35;
/// White lift added on dark glass (light glass takes none: white on frost
/// lifts nothing, STYLE.md Light).
pub const STACKED_LIFT: f32 = 0.018;
/// Rim multiplier: the hairline is what separates the two layers.
pub const STACKED_RIM: f32 = 1.3;

/// Lens strength share of a pane by its shorter logical side. A small control
/// keeps its lens; a big sheet or card is bent only a hair at its rim, so the
/// backdrop's grid reads straight through it: `FLOOR + (1 - FLOOR) / (1 +
/// (short / LENS_SIZE)^3)`. Continuous and non-increasing in size.
pub const LENS_SIZE: f32 = 64.0;
pub const LENS_FLOOR: f32 = 0.15;
pub fn lens_scale(pane: &Pane) -> f32 {
    let r = pane.w.min(pane.h).max(0) as f32 / LENS_SIZE;
    LENS_FLOOR + (1.0 - LENS_FLOOR) / (1.0 + r * r * r)
}

impl Look {
    /// This look on glass laid over a window's own glass.
    pub fn stacked(self, light: bool, alpha: u8) -> Look {
        Look {
            rim: self.rim * STACKED_RIM,
            lift: if light {
                self.lift
            } else {
                self.lift + STACKED_LIFT * alpha as f32 / 255.0
            },
            shade: self.shade,
            smoke: if light { STACKED_SMOKE_LIGHT } else { STACKED_SMOKE },
        }
    }
}

/// A light-scheme pane whose shorter side is at least this many logical px is
/// a card (launcher, toast), not a control cell.
pub const SLAB_MIN_SHORT: i32 = 56;
/// How far such a card lifts toward white: the near-opaque near-white slab of the
/// iOS light references (the backdrop shows a few percent at most).
pub const SLAB_LIFT_LIGHT: f32 = 0.86;

/// The white lift of `pane` in the light scheme: `lift`, or the slab lift for a card.
pub fn slab_lift(pane: &Pane, light: bool, lift: f32) -> f32 {
    if light && pane.w.min(pane.h) >= SLAB_MIN_SHORT {
        if pane.alpha == 0 {
            lift
        } else {
            lift.max(SLAB_LIFT_LIGHT)
        }
    } else {
        lift
    }
}

/// A pane lying fully inside an earlier, larger pane of the same surface whose
/// shorter side is at least this many logical px is a CELL: a control sitting on
/// a sheet (search field, selected row, key cap, icon chip), drawn as a fill on
/// the sheet's finished glass rather than as glass of its own.
pub const CELL_PARENT_MIN_SHORT: i32 = 56;

fn contains(outer: &Pane, inner: &Pane) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

/// The index of pane `i`'s parent card, or `None` when it is not a cell. The
/// parent is the nearest earlier (lower in z) pane that contains it, is larger
/// and has a shorter side of at least [`CELL_PARENT_MIN_SHORT`]. Siblings that
/// merely overlap, a pane that sticks out, a smaller or later container and a
/// small container are not parents.
pub fn cell_parent(panes: &[Pane], i: usize) -> Option<usize> {
    let cell = panes.get(i)?;
    if cell.w <= 0 || cell.h <= 0 {
        return None;
    }
    (0..i).rev().find(|&j| {
        let p = &panes[j];
        p.w.min(p.h) >= CELL_PARENT_MIN_SHORT
            && i64::from(p.w) * i64::from(p.h) > i64::from(cell.w) * i64::from(cell.h)
            && contains(p, cell)
    })
}

/// A cell's fill: alpha of the scheme's fill colour, how strongly it fades
/// top to bottom, and the hairline strength.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellLook {
    pub fill: f32,
    pub grad: f32,
    pub rim: f32,
}

/// Fill alpha of a plain cell at rest, dark (white fill) and light (ink fill).
const CELL_FILL_D: f32 = 0.09;
const CELL_FILL_L: f32 = 0.03;
/// The extra a focused or selected cell takes, and hover and press steps.
const CELL_FOCUS_D: f32 = 0.085;
const CELL_FOCUS_L: f32 = 0.03;
const CELL_HOT: f32 = 0.03;
const CELL_PRESS: f32 = 0.05;
/// Gradient strength: plain, focused.
const CELL_GRAD_PLAIN: f32 = 0.5;
const CELL_GRAD_FOCUS: f32 = 0.9;

/// The look of a cell, scaled by the pane's alpha; `None` when it draws nothing
/// (the same cases as [`look`]).
pub fn cell_look(pane: &Pane, light: bool) -> Option<CellLook> {
    let l = look(pane, light)?;
    let pressed = pane.state & PRESSED != 0;
    let hot = pressed || pane.state & HOVERED != 0;
    let focus = pane.tone == Tone::Focused;
    let a = pane.alpha as f32 / 255.0;
    let mut fill = if light { CELL_FILL_L } else { CELL_FILL_D };
    if pane.tone == Tone::Away {
        fill *= 0.5;
    }
    if focus {
        fill += if light { CELL_FOCUS_L } else { CELL_FOCUS_D };
    }
    if hot {
        fill += if pressed { CELL_PRESS } else { CELL_HOT };
    }
    Some(CellLook {
        fill: fill * a,
        grad: if focus { CELL_GRAD_FOCUS } else { CELL_GRAD_PLAIN },
        rim: l.rim,
    })
}

/// How far a cell's contact shadow may reach, logical px: its own reach, no
/// further than the nearest edge of its parent, so it never leaves the card.
pub const CELL_SHADOW_REACH: i32 = 5;
/// The contact shadow's alpha under a cell: dark, and the wider-and-fainter light.
pub const CELL_SHADOW_ALPHA_DARK: f32 = 0.45;
pub const CELL_SHADOW_ALPHA_LIGHT: f32 = 0.22;

pub fn cell_shadow_margin(cell: &Pane, parent: &Pane) -> i32 {
    let to_edges = [
        cell.x - parent.x,
        cell.y - parent.y,
        parent.x + parent.w - (cell.x + cell.w),
        parent.y + parent.h - (cell.y + cell.h),
    ];
    to_edges
        .into_iter()
        .min()
        .unwrap_or(0)
        .clamp(0, CELL_SHADOW_REACH)
}

/// The look of `pane`, or `None` when it draws nothing (latent at rest, or
/// fully transparent). Everything scales by `alpha / 255`. `light` selects the
/// light scheme's table; `false` is the dark table, unchanged.
pub fn look(pane: &Pane, light: bool) -> Option<Look> {
    let pressed = pane.state & PRESSED != 0;
    let hot = pressed || pane.state & HOVERED != 0;
    if pane.alpha == 0 || (pane.tone == Tone::Latent && !hot) {
        return None;
    }
    let (rim, lift, shade) = if light {
        let shade = match (pane.tone, hot, pressed) {
            (Tone::Focused, _, _) | (Tone::Away, false, _) => 0.0,
            (_, false, _) => LIGHT_REST,
            (_, true, false) => LIGHT_HOVER,
            (_, true, true) => LIGHT_PRESS,
        };
        let rim = match (pane.tone, hot) {
            (Tone::Plain | Tone::Latent, false) => LIGHT_RIM_REST,
            (Tone::Plain | Tone::Latent | Tone::Away, true) => LIGHT_RIM_HOT,
            (Tone::Away, false) => LIGHT_RIM_AWAY,
            (Tone::Focused, false) => LIGHT_RIM_FOCUS,
            (Tone::Focused, true) => LIGHT_RIM_FOCUS_HOT,
        };
        // The white lift stays on the focused pane only (white on frost lifts
        // nothing a pointer should not): the dark values, which are small.
        let lift = match (pane.tone, pressed, hot) {
            (Tone::Focused, _, false) => 0.014,
            (Tone::Focused, true, true) => 0.028,
            (Tone::Focused, false, true) => 0.02,
            _ => 0.0,
        };
        (rim, lift, shade)
    } else {
        let (rim, lift) = match (pane.tone, hot) {
            (Tone::Latent, false) => return None,
            (Tone::Plain, false) => (RIM_PLAIN, 0.0),
            (Tone::Plain | Tone::Latent, true) => (RIM_PLAIN_HOVER, if pressed { 0.02 } else { 0.012 }),
            (Tone::Away, false) => (RIM_AWAY, 0.0),
            (Tone::Away, true) => (RIM_PLAIN_HOVER * RIM_AWAY, if pressed { 0.02 } else { 0.012 }),
            (Tone::Focused, false) => (RIM_FOCUS, 0.014),
            (Tone::Focused, true) => (RIM_FOCUS_HOVER, if pressed { 0.028 } else { 0.02 }),
        };
        (rim, lift, 0.0)
    };
    let a = pane.alpha as f32 / 255.0;
    Some(Look {
        rim: rim * a,
        lift: lift * a,
        shade: shade * a,
        smoke: 1.0,
    })
}

/// [`look`] for a pane on glass that is already a window's (see
/// [`STACKED_SMOKE`]), or on bare backdrop when `stacked` is false.
pub fn look_on(pane: &Pane, light: bool, stacked: bool) -> Option<Look> {
    let l = look(pane, light)?;
    Some(if stacked { l.stacked(light, pane.alpha) } else { l })
}

/// A logical box placed at the surface origin `loc` in physical pixels.
fn place(
    logical: Rectangle<i32, Logical>,
    loc: smithay::utils::Point<i32, Physical>,
    scale: smithay::utils::Scale<f64>,
) -> Option<Rectangle<i32, Physical>> {
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

/// The pane's whole box, not cut to the surface: the shape a `CLIPPED` pane's
/// corners, rim and bezel are computed from.
pub fn pane_full_rect(
    pane: &Pane,
    loc: smithay::utils::Point<i32, Physical>,
    scale: smithay::utils::Scale<f64>,
) -> Option<Rectangle<i32, Physical>> {
    place(
        Rectangle::<i32, Logical>::new((pane.x, pane.y).into(), (pane.w, pane.h).into()),
        loc,
        scale,
    )
}

/// The set's clip in output-local physical pixels, cut to the surface. `None`
/// when the set has no clip; `Some(None)` when the clip misses the surface
/// (so every clipped pane is hidden).
pub fn clip_rect(
    clip: Option<Rectangle<i32, Logical>>,
    size: smithay::utils::Size<i32, Logical>,
    loc: smithay::utils::Point<i32, Physical>,
    scale: smithay::utils::Scale<f64>,
) -> Option<Option<Rectangle<i32, Physical>>> {
    let clip = clip?;
    Some(
        clip.intersection(Rectangle::from_size(size))
            .and_then(|l| place(l, loc, scale)),
    )
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
    place(logical, loc, scale)
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

/// A pane's contact shadow: the shell cell's drop (ec-ui `bar::CELL_SHADOW_Y`
/// 1, `CELL_SHADOW_BLUR` 6), drawn by the window shadow program at its rest
/// weight. Logical px.
pub const SHADOW_DROP: i32 = 2;
pub const SHADOW_RANGE: i32 = 10;
/// The light scheme's shadow is wide and faint (iOS light: separation from a
/// soft halo, not a border); the dark one stays tight.
pub const SHADOW_DROP_LIGHT: i32 = 6;
pub const SHADOW_RANGE_LIGHT: i32 = 26;
/// Share of the pane's alpha the light shadow keeps.
pub const SHADOW_ALPHA_LIGHT: f32 = 0.38;

/// `(range, drop)` in logical px for the scheme.
pub fn shadow_extent(light: bool) -> (i32, i32) {
    if light {
        (SHADOW_RANGE_LIGHT, SHADOW_DROP_LIGHT)
    } else {
        (SHADOW_RANGE, SHADOW_DROP)
    }
}

/// Whether `pane` casts a contact shadow, by the shell cell's rule
/// (`ec-ui` `CellTone`): none while pressed (pushed into the glass), none for
/// a minimized or latent cell at rest, none at zero alpha. Everything else,
/// focused included, casts the same rest shadow.
pub fn casts_shadow(pane: &Pane) -> bool {
    let pressed = pane.state & PRESSED != 0;
    let hovered = pane.state & HOVERED != 0;
    let resting = matches!(pane.tone, Tone::Away | Tone::Latent) && !hovered;
    pane.alpha != 0 && !pressed && !resting
}

/// The shadow's area, logical: the cell's box grown by `SHADOW_RANGE` on the
/// sides and top, and by `SHADOW_DROP` more at the bottom. The shader's
/// `size` is this area, so its `range` and `drop` are in the same units.
pub fn shadow_area(cell: Rectangle<i32, Logical>, light: bool) -> Rectangle<i32, Logical> {
    let (range, drop) = shadow_extent(light);
    Rectangle::new(
        (cell.loc.x - range, cell.loc.y - range).into(),
        (cell.size.w + 2 * range, cell.size.h + 2 * range + drop).into(),
    )
}

/// The shadow shader's `[range, radius, drop]` for a cell of corner `radius`
/// (logical px, already grown by any window animation scale).
pub fn shadow_params(radius: f32, light: bool) -> [f32; 3] {
    let (range, drop) = shadow_extent(light);
    [range as f32, radius, drop as f32]
}

/// `of` entry for a pane that draws on its own.
pub const ALONE: u8 = u8::MAX;

/// A joined shape (ADR 0130): up to [`MAX_SHAPE_PANES`] panes drawn as one
/// piece of glass. `members` are pane indices in attachment order: the lowest
/// index first, then each pane reached over a join from one before it, so the
/// shape is drawn at its lowest index. `fillet[k]` is the radius of the join
/// member `k` was reached by (0 for the first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinedShape {
    pub members: [u8; MAX_SHAPE_PANES],
    pub fillet: [u32; MAX_SHAPE_PANES],
    pub len: usize,
}

impl JoinedShape {
    /// The member indices, in attachment order.
    pub fn members(&self) -> &[u8] {
        &self.members[..self.len]
    }
}

/// The joined shapes of a pane list (see [`find_shapes`]): the shapes in index
/// order, and for each pane the shape it belongs to, or [`ALONE`]. Reused
/// across frames: `find_shapes` clears, not reallocates, it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shapes {
    pub list: Vec<JoinedShape>,
    pub of: [u8; MAX_PANES],
}

impl Default for Shapes {
    fn default() -> Self {
        Self {
            list: Vec::new(),
            of: [ALONE; MAX_PANES],
        }
    }
}

/// Whether pane `i` may take part in a joined shape. It must draw glass on its
/// own: a box with some alpha and a look (a latent pane at rest draws nothing),
/// and not a cell (a fill on its card, see [`cell_parent`]). A pane under a
/// `set_clip` cut is not joined either: the cut is not a union's edge. The
/// `drawn` filter of [`find_shapes`] adds what the renderer also needs.
pub fn joinable(panes: &[Pane], i: usize, clipped: bool) -> bool {
    let Some(p) = panes.get(i) else {
        return false;
    };
    p.w > 0
        && p.h > 0
        && look(p, false).is_some()
        && !(clipped && p.state & CLIPPED != 0)
        && cell_parent(panes, i).is_none()
}

/// Two boxes touch when they overlap or share an edge (closed rectangles
/// intersect). A join between boxes that do not touch has no fillet to span,
/// so it draws as two panes (ADR 0130).
fn touching(a: &Pane, b: &Pane) -> bool {
    let (ax0, ay0) = (i64::from(a.x), i64::from(a.y));
    let (ax1, ay1) = (ax0 + i64::from(a.w), ay0 + i64::from(a.h));
    let (bx0, by0) = (i64::from(b.x), i64::from(b.y));
    let (bx1, by1) = (bx0 + i64::from(b.w), by0 + i64::from(b.h));
    ax0 <= bx1 && bx0 <= ax1 && ay0 <= by1 && by0 <= ay1
}

/// The panes reached from `root` over `edges` (valid joins whose ends are kept),
/// in breadth-first order, each with the join radius it was reached by. Returns
/// how many were reached; `order` holds them from index 0.
fn reach(
    root: usize,
    edges: &[(u8, u8, u32)],
    keep: &[bool; MAX_PANES],
    order: &mut [(u8, u32); MAX_PANES],
) -> usize {
    let mut seen = [false; MAX_PANES];
    order[0] = (root as u8, 0);
    seen[root] = true;
    let (mut len, mut head) = (1, 0);
    while head < len {
        let u = order[head].0 as usize;
        head += 1;
        for &(a, b, radius) in edges {
            let (a, b) = (a as usize, b as usize);
            let v = if a == u {
                b
            } else if b == u {
                a
            } else {
                continue;
            };
            if keep[v] && !seen[v] {
                seen[v] = true;
                order[len] = (v as u8, radius);
                len += 1;
            }
        }
    }
    len
}

/// Group the panes of a surface into joined shapes (ADR 0130), into `out`.
///
/// A join counts when both panes are [`joinable`] and drawn (`drawn`), are
/// distinct, and touch. The panes joined through those form components; a
/// component of two or more is one shape. A component larger than
/// [`MAX_SHAPE_PANES`] keeps its lowest sixteen indices as the shape and the
/// rest draw alone (a limit, not an error). Duplicate joins are harmless.
pub fn find_shapes(
    panes: &[Pane],
    joins: &[Join],
    clipped: bool,
    drawn: impl Fn(usize) -> bool,
    out: &mut Shapes,
) {
    out.list.clear();
    out.of = [ALONE; MAX_PANES];
    let n = panes.len().min(MAX_PANES);
    let mut keep = [false; MAX_PANES];
    for (i, k) in keep.iter_mut().enumerate().take(n) {
        *k = drawn(i) && joinable(panes, i, clipped);
    }
    let mut edges = [(0u8, 0u8, 0u32); MAX_JOINS];
    let mut m = 0;
    for j in joins {
        let (a, b) = (j.a as usize, j.b as usize);
        if a < n && b < n && a != b && keep[a] && keep[b] && touching(&panes[a], &panes[b]) {
            edges[m] = (j.a, j.b, j.radius);
            m += 1;
        }
    }
    let edges = &edges[..m];
    if edges.is_empty() {
        return;
    }
    let mut seen = [false; MAX_PANES];
    let mut order = [(0u8, 0u32); MAX_PANES];
    for root in 0..n {
        if !keep[root] || seen[root] {
            continue;
        }
        let mut len = reach(root, edges, &keep, &mut order);
        if len > MAX_SHAPE_PANES {
            // The lowest sixteen keep their joins; the others draw alone. The
            // root is the component's lowest index, so it stays.
            let mut idx = [0u8; MAX_PANES];
            for (slot, &(p, _)) in idx.iter_mut().zip(&order[..len]) {
                *slot = p;
            }
            idx[..len].sort_unstable();
            for &p in &idx[MAX_SHAPE_PANES..len] {
                keep[p as usize] = false;
            }
            len = reach(root, edges, &keep, &mut order);
        }
        for &(p, _) in &order[..len] {
            seen[p as usize] = true;
        }
        if len < 2 {
            continue;
        }
        let mut shape = JoinedShape {
            members: [0; MAX_SHAPE_PANES],
            fillet: [0; MAX_SHAPE_PANES],
            len,
        };
        for (k, &(p, radius)) in order[..len].iter().enumerate() {
            shape.members[k] = p;
            shape.fillet[k] = radius;
        }
        let id = out.list.len() as u8;
        for &(p, _) in &order[..len] {
            out.of[p as usize] = id;
        }
        out.list.push(shape);
    }
}

/// One member box for [`joined_distance`]: `[x, y, w, h]` (the shape box,
/// gl px), its corner radius and the fillet it joins by.
pub type MemberBox = ([f32; 4], f32, f32);

/// `(distance, arc centre)` of one rounded box at `f`: the CPU mirror of the
/// joined shader's per-member box (effects.rs `shape_sdg`).
fn box_arc(b: [f32; 4], radius: f32, f: (f32, f32)) -> (f32, (f32, f32)) {
    let (hx, hy) = (b[2] * 0.5, b[3] * 0.5);
    let r = radius.min(hx.min(hy)).max(0.0);
    let qx = (f.0 - (b[0] + hx)).abs() - hx + r;
    let qy = (f.1 - (b[1] + hy)).abs() - hy + r;
    let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
    let arc = (
        f.0.max(b[0] + r).min(b[0] + b[2] - r),
        f.1.max(b[1] + r).min(b[1] + b[3] - r),
    );
    (d, arc)
}

/// `blur::join` at one member's own fillet and corner radius: the joined
/// shader's `join_member`, distance only.
fn smooth_join(a: (f32, (f32, f32)), b: (f32, (f32, f32)), fillet: f32, radius: f32) -> (f32, (f32, f32)) {
    let m = if b.0 < a.0 { b } else { a };
    if fillet <= 0.0 {
        return m;
    }
    let h = (fillet - (a.0 - b.0).abs()).max(0.0) / fillet;
    let gap = ((a.1 .0 - b.1 .0).hypot(a.1 .1 - b.1 .1) / radius.max(1.0)).min(1.0);
    (m.0 - gap * h * h * fillet * 0.25, m.1)
}

/// The signed distance of a joined element at `f` (ADR 0130): the members
/// folded in order, each smooth-min'd into the shape so far at its own fillet.
/// Mirrors the joined shader; with every fillet 0 it is the plain union.
pub fn joined_distance(members: &[MemberBox], f: (f32, f32)) -> f32 {
    let mut acc: Option<(f32, (f32, f32))> = None;
    for &(b, radius, fillet) in members {
        let c = box_arc(b, radius, f);
        acc = Some(match acc {
            None => c,
            Some(a) => smooth_join(a, c, fillet, radius),
        });
    }
    acc.map_or(f32::INFINITY, |a| a.0)
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

    fn join_rec(a: u32, b: u32, radius: u32) -> Vec<u8> {
        JoinRecord { a, b, radius }.to_bytes().to_vec()
    }

    /// ADR 0130: valid joins parse in order; the radius is capped at 64; an
    /// empty array is no joins.
    #[test]
    fn parses_joins_and_caps_the_radius() {
        let set = parse_joins(&[join_rec(0, 1, 16), join_rec(2, 1, 900)].concat(), 3).unwrap();
        assert_eq!(
            set.as_slice(),
            &[
                Join {
                    a: 0,
                    b: 1,
                    radius: 16
                },
                Join {
                    a: 2,
                    b: 1,
                    radius: MAX_JOIN_RADIUS
                },
            ]
        );
        assert!(parse_joins(&[], 3).unwrap().is_empty());
    }

    /// `bad_join`: a length that is not whole records, an index at or past the
    /// pane count, and a pane joined to itself. `too_many_joins` past 64.
    #[test]
    fn bad_joins_are_refused_and_the_cap_is_64() {
        assert_eq!(parse_joins(&[0u8; 11], 4), Err(JoinError::BadJoin));
        assert_eq!(parse_joins(&join_rec(0, 4, 8), 4), Err(JoinError::BadJoin));
        assert_eq!(parse_joins(&join_rec(4, 0, 8), 4), Err(JoinError::BadJoin));
        assert_eq!(parse_joins(&join_rec(1, 1, 8), 4), Err(JoinError::BadJoin));
        assert_eq!(parse_joins(&join_rec(0, 1, 8), 0), Err(JoinError::BadJoin));
        assert_eq!(
            parse_joins(&join_rec(0, 0xffff_ffff, 8), 64),
            Err(JoinError::BadJoin)
        );
        let ok: Vec<u8> = join_rec(0, 1, 8).into_iter().cycle().take(12 * 64).collect();
        assert_eq!(parse_joins(&ok, 2).unwrap().len(), 64);
        let over: Vec<u8> = join_rec(0, 1, 8).into_iter().cycle().take(12 * 65).collect();
        assert_eq!(parse_joins(&over, 2), Err(JoinError::TooManyJoins));
    }

    /// Joins belong to the pane list they were sent with: they are part of
    /// the set's identity, and a fresh set has none.
    #[test]
    fn a_pane_set_carries_its_joins() {
        let mut set =
            parse_panes(&[rec(0, 0, 10, 10, 0, 0, 255), rec(10, 0, 10, 10, 0, 0, 255)].concat()).unwrap();
        assert!(set.joins().is_empty());
        let plain = set.clone();
        set.set_joins(parse_joins(&join_rec(0, 1, 12), set.len()).unwrap());
        assert_eq!(
            set.joins(),
            &[Join {
                a: 0,
                b: 1,
                radius: 12
            }]
        );
        assert_ne!(set, plain, "joins are part of the set's identity");
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
    fn nesting_is_a_cell_inside_a_larger_early_card() {
        let card = Pane {
            x: 0,
            y: 0,
            w: 400,
            h: 300,
            ..pane(Tone::Plain, 0, 255)
        };
        let field = Pane {
            x: 20,
            y: 20,
            w: 360,
            h: 40,
            ..pane(Tone::Plain, 0, 255)
        };
        let row = Pane {
            x: 20,
            y: 80,
            w: 360,
            h: 36,
            ..pane(Tone::Focused, 0, 255)
        };
        let sibling = Pane {
            x: 380,
            y: 0,
            w: 300,
            h: 300,
            ..pane(Tone::Plain, 0, 255)
        };
        let sticks_out = Pane {
            x: 300,
            y: 20,
            w: 200,
            h: 40,
            ..pane(Tone::Plain, 0, 255)
        };
        let small_card = Pane {
            x: 700,
            y: 0,
            w: 120,
            h: 40,
            ..pane(Tone::Plain, 0, 255)
        };
        let in_small = Pane {
            x: 710,
            y: 5,
            w: 30,
            h: 30,
            ..pane(Tone::Plain, 0, 255)
        };
        let set = [card, field, row, sibling, sticks_out, small_card, in_small];
        assert_eq!(cell_parent(&set, 0), None, "the card itself");
        assert_eq!(cell_parent(&set, 1), Some(0));
        assert_eq!(cell_parent(&set, 2), Some(0));
        assert_eq!(cell_parent(&set, 3), None, "a stack-peek sibling is no cell");
        assert_eq!(cell_parent(&set, 4), None, "partial overlap");
        assert_eq!(cell_parent(&set, 5), None, "a pane outside every card");
        assert_eq!(cell_parent(&set, 6), None, "its container is under 56 px");
        assert_eq!(cell_parent(&set, 99), None);
    }

    #[test]
    fn a_container_must_come_first_and_be_larger() {
        let a = Pane {
            x: 0,
            y: 0,
            w: 300,
            h: 100,
            ..pane(Tone::Plain, 0, 255)
        };
        let b = Pane {
            x: 10,
            y: 10,
            w: 100,
            h: 60,
            ..pane(Tone::Plain, 0, 255)
        };
        // The container after the contained one is not a parent (z order).
        assert_eq!(cell_parent(&[b, a], 0), None);
        assert_eq!(cell_parent(&[b, a], 1), None);
        assert_eq!(cell_parent(&[a, b], 1), Some(0));
        // The same box is not a cell of itself or of an equal pane.
        assert_eq!(cell_parent(&[a, a], 1), None);
        // The threshold is on the shorter side: 56 qualifies, 55 does not.
        let slim = |h| Pane {
            x: 0,
            y: 0,
            w: 300,
            h,
            ..pane(Tone::Plain, 0, 255)
        };
        let inner = Pane {
            x: 5,
            y: 5,
            w: 20,
            h: 20,
            ..pane(Tone::Plain, 0, 255)
        };
        assert_eq!(cell_parent(&[slim(56), inner], 1), Some(0));
        assert_eq!(cell_parent(&[slim(55), inner], 1), None);
    }

    #[test]
    fn a_cell_fills_less_than_it_would_glass_and_never_beyond_its_card() {
        let plain = Pane {
            w: 100,
            h: 30,
            ..pane(Tone::Plain, 0, 255)
        };
        let sel = Pane {
            tone: Tone::Focused,
            ..plain
        };
        for light in [false, true] {
            let (p, f) = (cell_look(&plain, light).unwrap(), cell_look(&sel, light).unwrap());
            assert!(p.fill > 0.0 && p.fill < 0.2, "faint");
            assert!(
                f.fill > p.fill && f.grad > p.grad && f.rim > p.rim,
                "the selected row is clearer"
            );
        }
        assert_eq!(cell_look(&Pane { alpha: 0, ..plain }, false), None);
        let card = Pane {
            x: 0,
            y: 0,
            w: 400,
            h: 300,
            ..plain
        };
        let near = Pane {
            x: 2,
            y: 100,
            w: 50,
            h: 30,
            ..plain
        };
        let far = Pane {
            x: 100,
            y: 100,
            w: 50,
            h: 30,
            ..plain
        };
        assert_eq!(cell_shadow_margin(&near, &card), 2);
        assert_eq!(cell_shadow_margin(&far, &card), CELL_SHADOW_REACH);
    }

    /// A light card (launcher, toast) lifts to the near-white slab; a control
    /// cell and the dark scheme keep their lift.
    #[test]
    fn a_light_card_is_a_slab_and_a_cell_is_not() {
        let card = Pane {
            w: 440,
            h: 110,
            ..pane(Tone::Plain, 0, 255)
        };
        let cell = Pane {
            w: 120,
            h: 36,
            ..pane(Tone::Plain, 0, 255)
        };
        assert!((slab_lift(&card, true, 0.0) - SLAB_LIFT_LIGHT).abs() < 1e-6);
        assert_eq!(slab_lift(&cell, true, 0.01), 0.01);
        assert_eq!(slab_lift(&card, false, 0.0), 0.0);
        // A faint-fill card is still a slab (its alpha is the fill, not the card), a transparent
        // one draws nothing, and a larger lift is never lowered.
        let faint = Pane { alpha: 60, ..card };
        assert!((slab_lift(&faint, true, 0.0) - SLAB_LIFT_LIGHT).abs() < 1e-6);
        assert_eq!(slab_lift(&Pane { alpha: 0, ..card }, true, 0.0), 0.0);
        assert_eq!(slab_lift(&card, true, 0.95), 0.95);
    }

    /// The shader's two mixes for one channel, in its order: white lift, then
    /// the ink shade (`effects::GLASS_BODY`). A CPU mirror for the look tests.
    fn over(backdrop: f32, look: Look) -> f32 {
        const INK: f32 = 0.102; // `LIFT_L`, the ink's red channel
        let lifted = backdrop + (1.0 - backdrop) * look.lift;
        lifted * (1.0 - look.shade) + INK * look.shade
    }

    #[test]
    fn tone_and_state_map_to_rim_and_lift() {
        let plain = look(&pane(Tone::Plain, 0, 255), false).unwrap();
        let focus = look(&pane(Tone::Focused, 0, 255), false).unwrap();
        let away = look(&pane(Tone::Away, 0, 255), false).unwrap();
        let hover = look(&pane(Tone::Plain, HOVERED, 255), false).unwrap();
        let press = look(&pane(Tone::Plain, PRESSED, 255), false).unwrap();
        assert!(focus.rim > hover.rim && hover.rim > plain.rim && plain.rim > away.rim);
        assert_eq!(away.lift, 0.0);
        assert_eq!(plain.lift, 0.0);
        assert!(press.lift > hover.lift && hover.lift > 0.0);
        // Dark has no ink step at all: its look is what it was before.
        for l in [plain, focus, away, hover, press] {
            assert_eq!(l.shade, 0.0);
        }
        assert!((focus.rim / plain.rim - 2.5).abs() < 1e-6);
        assert!(look(&pane(Tone::Latent, 0, 255), false).is_none());
        assert_eq!(look(&pane(Tone::Latent, HOVERED, 255), false), Some(hover));
    }

    #[test]
    fn alpha_scales_everything() {
        for light in [false, true] {
            let full = look(&pane(Tone::Focused, HOVERED, 255), light).unwrap();
            let half = look(&pane(Tone::Focused, HOVERED, 51), light).unwrap();
            assert!((half.rim - full.rim * 0.2).abs() < 1e-5);
            assert!((half.lift - full.lift * 0.2).abs() < 1e-6);
            assert!((half.shade - full.shade * 0.2).abs() < 1e-6);
            assert!(look(&pane(Tone::Plain, 0, 0), light).is_none());
        }
    }

    #[test]
    fn light_rest_hover_press_step_toward_the_ink() {
        let rest = look(&pane(Tone::Plain, 0, 255), true).unwrap();
        let hover = look(&pane(Tone::Plain, HOVERED, 255), true).unwrap();
        let press = look(&pane(Tone::Plain, PRESSED, 255), true).unwrap();
        assert!((rest.shade - 0.05).abs() < 1e-6);
        assert!((hover.shade - 0.09).abs() < 1e-6);
        assert!((press.shade - 0.13).abs() < 1e-6);
        // Rest < hover < press, and each is a darker step than the last.
        assert!(rest.shade < hover.shade && hover.shade < press.shade);
        // No white on a plain light pane: white over frost lifts it off its column.
        assert_eq!(rest.lift, 0.0);
        assert_eq!(hover.lift, 0.0);
        assert_eq!(press.lift, 0.0);
        // The hairline: .12 at rest, .18 hovered, all in units of the base lobe
        // (rim * LINE_BASE at the default glass.rim 0.5 is the alpha).
        assert!((rest.rim * 0.05 * 2.0 * 0.5 - 0.12).abs() < 1e-5);
        assert!((hover.rim * 0.05 * 2.0 * 0.5 - 0.18).abs() < 1e-5);
        assert_eq!(press.rim, hover.rim);
    }

    #[test]
    fn light_focus_is_the_one_lift_and_takes_no_ink() {
        let plain = look(&pane(Tone::Plain, 0, 255), true).unwrap();
        for state in [0, HOVERED, PRESSED] {
            let focus = look(&pane(Tone::Focused, state, 255), true).unwrap();
            assert_eq!(focus.shade, 0.0);
            assert!(focus.lift > 0.0);
            // Lifted: brighter than a plain rest pane on the same backdrop.
            assert!(over(0.97, focus) > over(0.97, plain));
        }
        // The focused rest hairline is the cell's .18, and it is above the plain one.
        let focus = look(&pane(Tone::Focused, 0, 255), true).unwrap();
        assert!((focus.rim * 0.05 - 0.18).abs() < 1e-5);
        assert!(focus.rim > plain.rim);
    }

    #[test]
    fn light_look_darkens_its_backdrop_and_orders_rest_hover_press() {
        for backdrop in [0.93f32, 0.97, 0.99] {
            let rest = over(backdrop, look(&pane(Tone::Plain, 0, 255), true).unwrap());
            let hover = over(backdrop, look(&pane(Tone::Plain, HOVERED, 255), true).unwrap());
            let press = over(backdrop, look(&pane(Tone::Plain, PRESSED, 255), true).unwrap());
            assert!(rest < backdrop, "rest fill must darken {backdrop}");
            assert!(
                hover < rest && press < hover,
                "rest > hover > press darkness order"
            );
        }
        // An away (minimized) chip has no fill at rest: it sits on the backdrop.
        let away = over(0.97, look(&pane(Tone::Away, 0, 255), true).unwrap());
        assert_eq!(away, 0.97);
    }

    #[test]
    fn dark_is_unchanged_by_the_light_table() {
        // A dark pane's shade is zero, so the ink mix is the identity and the
        // white lift is the only thing the shader adds: a dark pane lightens.
        for (tone, state) in [(Tone::Plain, 0), (Tone::Plain, HOVERED), (Tone::Focused, PRESSED)] {
            let l = look(&pane(tone, state, 255), false).unwrap();
            assert_eq!(l.shade, 0.0);
            assert!(over(0.2, l) >= 0.2);
        }
        assert_eq!(over(0.2, look(&pane(Tone::Plain, 0, 255), false).unwrap()), 0.2);
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

    #[test]
    fn cells_cast_the_shell_cell_shadow() {
        // Plain and focused cells cast one at rest; minimized and latent ones
        // only when lit, as the shell's cell does.
        assert!(casts_shadow(&pane(Tone::Plain, 0, 255)));
        assert!(casts_shadow(&pane(Tone::Focused, 0, 255)));
        assert!(!casts_shadow(&pane(Tone::Away, 0, 255)));
        assert!(!casts_shadow(&pane(Tone::Latent, 0, 255)));
        assert!(casts_shadow(&pane(Tone::Away, HOVERED, 255)));
        assert!(casts_shadow(&pane(Tone::Latent, HOVERED, 255)));
        assert!(casts_shadow(&pane(Tone::Plain, HOVERED, 255)));
        // Pressed casts none, whatever the tone; so does a transparent pane.
        for tone in [Tone::Plain, Tone::Focused, Tone::Away, Tone::Latent] {
            assert!(!casts_shadow(&pane(tone, PRESSED, 255)));
            assert!(!casts_shadow(&pane(tone, HOVERED | PRESSED, 255)));
        }
        assert!(!casts_shadow(&pane(Tone::Plain, 0, 0)));
    }

    #[test]
    fn the_shadow_hugs_the_cell_and_drops_a_pixel() {
        let cell = Rectangle::<i32, Logical>::new((20, 10).into(), (80, 30).into());
        let area = shadow_area(cell, false);
        // Ten px out on the left, right and top; twelve at the bottom: two px down.
        assert_eq!((area.loc.x, area.loc.y), (10, 0));
        assert_eq!((area.size.w, area.size.h), (100, 52));
        assert_eq!(area.loc.x + area.size.w, cell.loc.x + cell.size.w + SHADOW_RANGE);
        assert_eq!(
            area.loc.y + area.size.h,
            cell.loc.y + cell.size.h + SHADOW_RANGE + SHADOW_DROP
        );
        // Nothing reaches further than the range and drop, whatever the cell.
        let tiny = Rectangle::<i32, Logical>::new((0, 0).into(), (1, 1).into());
        let a = shadow_area(tiny, false);
        assert!(a.loc.x >= -SHADOW_RANGE && a.loc.y >= -SHADOW_RANGE);
        assert_eq!(a.size.w - tiny.size.w, 2 * SHADOW_RANGE);
        assert_eq!(a.size.h - tiny.size.h, 2 * SHADOW_RANGE + SHADOW_DROP);
        assert_eq!(shadow_params(8.0, false), [10.0, 8.0, 2.0]);
        assert_eq!(shadow_params(8.0, true), [26.0, 8.0, 6.0]);
        let l = shadow_area(cell, true);
        assert_eq!((l.size.w, l.size.h), (80 + 52, 30 + 52 + 6));
    }

    #[test]
    fn light_casts_warm_ink_and_dark_casts_black() {
        // The colour is the window shadow's, passed in by the caller.
        let light = ec_abyss_config::Appearance {
            scheme: ec_theme::Scheme::Light,
            ..Default::default()
        };
        let [r, g, b] = light.shadow_rgb();
        assert!(r > g && g > b && r < 0.3, "{:?}", [r, g, b]);
        assert_eq!(ec_abyss_config::Appearance::default().shadow_rgb(), [0.0; 3]);
    }

    #[test]
    fn stacked_glass_keeps_the_rim_and_thins_the_smoke() {
        for light in [false, true] {
            let p = pane(Tone::Plain, 0, 255);
            let flat = look_on(&p, light, false).unwrap();
            assert_eq!(flat, look(&p, light).unwrap());
            assert_eq!(flat.smoke, 1.0);
            let st = look_on(&p, light, true).unwrap();
            assert!(st.smoke < 0.6 && st.smoke > 0.0, "weaker, not gone");
            assert!(st.rim > flat.rim, "the rim separates the two layers");
            assert_eq!(st.shade, flat.shade);
            assert_eq!(st.lift > flat.lift, !light, "white lift on dark only");
        }
    }

    #[test]
    fn stacked_look_follows_alpha_and_never_draws_latent_panes() {
        let half = pane(Tone::Plain, 0, 128);
        let a = look_on(&half, false, true).unwrap();
        let full = look_on(&pane(Tone::Plain, 0, 255), false, true).unwrap();
        assert!(a.lift < full.lift && a.lift > 0.0);
        assert_eq!(look_on(&pane(Tone::Latent, 0, 255), false, true), None);
        assert_eq!(look_on(&pane(Tone::Plain, 0, 0), false, true), None);
    }

    #[test]
    fn big_panes_are_barely_lensed_and_small_ones_keep_theirs() {
        let sized = |w: i32, h: i32| Pane {
            w,
            h,
            ..Pane::default()
        };
        // The largest bend any pane gets is the 4 px cap times this share.
        let px = |p: &Pane| 4.0 * lens_scale(p);
        assert!(px(&sized(32, 32)) > 3.5, "a key cap keeps its lens");
        assert!(px(&sized(300, 36)) > 3.2, "a row is as tall as a control");
        assert!(px(&sized(120, 100)) < 1.4, "a toast card is bent about a pixel");
        assert!(px(&sized(540, 374)) <= 0.7, "a sheet is bent under a pixel");
        assert!(px(&sized(4000, 3000)) >= 4.0 * LENS_FLOOR - 1e-4);
        // Smooth and non-increasing in size.
        let mut last = f32::MAX;
        for s in 1..800 {
            let v = lens_scale(&sized(s, s));
            assert!(v <= last + 1e-6);
            last = v;
        }
    }

    /// A plain, drawn box at (x, y) of size w x h.
    fn boxed(x: i32, y: i32, w: i32, h: i32) -> Pane {
        Pane {
            x,
            y,
            w,
            h,
            ..pane(Tone::Plain, 0, 255)
        }
    }

    fn join(a: u8, b: u8, radius: u32) -> Join {
        Join { a, b, radius }
    }

    fn shapes_of(panes: &[Pane], joins: &[Join]) -> Shapes {
        let mut out = Shapes::default();
        find_shapes(panes, joins, false, |_| true, &mut out);
        out
    }

    #[test]
    fn a_chain_is_one_shape_in_attachment_order() {
        // Three boxes in a row, each touching the next (shared edges).
        let panes = [boxed(0, 0, 10, 10), boxed(10, 0, 10, 10), boxed(20, 0, 10, 10)];
        let out = shapes_of(&panes, &[join(1, 2, 8), join(0, 1, 4)]);
        assert_eq!(out.list.len(), 1);
        let shape = out.list[0];
        assert_eq!(shape.members(), &[0, 1, 2]);
        // The fillet is the radius of the join each member was reached by.
        assert_eq!(&shape.fillet[..3], &[0, 4, 8]);
        assert_eq!(out.of[0], 0);
        assert_eq!(out.of[2], 0);
        assert_eq!(out.of[3], ALONE);
    }

    #[test]
    fn a_cycle_is_one_shape_with_each_member_once() {
        let panes = [boxed(0, 0, 10, 10), boxed(10, 0, 10, 10), boxed(0, 10, 10, 10)];
        let out = shapes_of(&panes, &[join(0, 1, 8), join(1, 2, 8), join(2, 0, 8)]);
        assert_eq!(out.list.len(), 1);
        assert_eq!(out.list[0].len, 3);
        let mut m = out.list[0].members().to_vec();
        m.sort_unstable();
        assert_eq!(m, vec![0, 1, 2]);
    }

    #[test]
    fn panes_that_do_not_touch_are_not_joined() {
        let panes = [boxed(0, 0, 10, 10), boxed(11, 0, 10, 10)];
        let out = shapes_of(&panes, &[join(0, 1, 8)]);
        assert!(out.list.is_empty());
        assert_eq!(out.of[0], ALONE);
        assert_eq!(out.of[1], ALONE);
    }

    #[test]
    fn touching_at_a_corner_counts() {
        let panes = [boxed(0, 0, 10, 10), boxed(10, 10, 10, 10)];
        let out = shapes_of(&panes, &[join(0, 1, 8)]);
        assert_eq!(out.list.len(), 1);
    }

    #[test]
    fn duplicate_self_and_out_of_range_joins_are_harmless() {
        let panes = [boxed(0, 0, 10, 10), boxed(10, 0, 10, 10)];
        let joins = [join(0, 1, 8), join(1, 0, 8), join(0, 0, 8), join(0, 9, 8)];
        let out = shapes_of(&panes, &joins);
        assert_eq!(out.list.len(), 1);
        assert_eq!(out.list[0].members(), &[0, 1]);
    }

    #[test]
    fn an_undrawn_pane_breaks_the_chain() {
        let panes = [boxed(0, 0, 10, 10), boxed(10, 0, 10, 10), boxed(20, 0, 10, 10)];
        let mut out = Shapes::default();
        find_shapes(
            &panes,
            &[join(0, 1, 8), join(1, 2, 8)],
            false,
            |i| i != 1,
            &mut out,
        );
        assert!(out.list.is_empty());
    }

    #[test]
    fn a_clipped_pane_is_not_joined() {
        let panes = [
            boxed(0, 0, 10, 10),
            Pane {
                state: CLIPPED,
                ..boxed(10, 0, 10, 10)
            },
        ];
        let mut out = Shapes::default();
        find_shapes(&panes, &[join(0, 1, 8)], true, |_| true, &mut out);
        assert!(out.list.is_empty());
        // Without a clip in force the same state is an ordinary pane.
        find_shapes(&panes, &[join(0, 1, 8)], false, |_| true, &mut out);
        assert_eq!(out.list.len(), 1);
    }

    #[test]
    fn a_zero_area_pane_is_not_joined() {
        let panes = [boxed(0, 0, 10, 10), boxed(10, 0, 0, 10)];
        let out = shapes_of(&panes, &[join(0, 1, 8)]);
        assert!(out.list.is_empty());
    }

    #[test]
    fn seventeen_members_keep_the_lowest_sixteen() {
        let panes: Vec<Pane> = (0..17).map(|i| boxed(i * 10, 0, 10, 10)).collect();
        let joins: Vec<Join> = (0..16).map(|i| join(i, i + 1, 8)).collect();
        let out = shapes_of(&panes, &joins);
        assert_eq!(out.list.len(), 1);
        assert_eq!(out.list[0].len, MAX_SHAPE_PANES);
        let expect: Vec<u8> = (0..16).collect();
        assert_eq!(out.list[0].members(), expect.as_slice());
        assert_eq!(out.of[15], 0);
        assert_eq!(out.of[16], ALONE);
    }

    #[test]
    fn find_shapes_reuses_its_output() {
        let panes = [boxed(0, 0, 10, 10), boxed(10, 0, 10, 10)];
        let mut out = Shapes::default();
        find_shapes(&panes, &[join(0, 1, 8)], false, |_| true, &mut out);
        find_shapes(&panes, &[], false, |_| true, &mut out);
        assert!(out.list.is_empty());
        assert_eq!(out.of[0], ALONE);
    }

    /// The pill and drawer of the headless probe (gl px): the pill across the
    /// top, the drawer hanging from its bottom-right, touching it.
    fn pill_and_drawer(fillet: f32) -> [MemberBox; 2] {
        [
            ([40.0, 40.0, 300.0, 40.0], 12.0, 0.0),
            ([290.0, 80.0, 50.0, 160.0], 12.0, fillet),
        ]
    }

    #[test]
    fn a_zero_fillet_is_the_plain_union() {
        let [a, b] = pill_and_drawer(0.0);
        for &p in &[(287.0, 83.0), (300.0, 100.0), (5.0, 5.0), (330.0, 230.0)] {
            let plain = box_arc(a.0, a.1, p).0.min(box_arc(b.0, b.1, p).0);
            assert_eq!(joined_distance(&[a, b], p), plain, "at {p:?}");
        }
    }

    #[test]
    fn far_from_the_join_the_union_is_the_plain_minimum() {
        // Far from both boxes the two distances differ by more than the fillet,
        // so the bite is zero and the joined distance is the plain minimum.
        let [a, b] = pill_and_drawer(16.0);
        for &p in &[(5.0, 5.0), (400.0, 20.0), (20.0, 300.0)] {
            let da = box_arc(a.0, a.1, p).0;
            let db = box_arc(b.0, b.1, p).0;
            assert!((da - db).abs() >= 16.0, "probe {p:?} is near the join");
            assert_eq!(joined_distance(&[a, b], p), da.min(db), "at {p:?}");
        }
    }

    #[test]
    fn the_inside_corner_fills_outside_both_boxes() {
        // The fillet puts glass in the concave corner: somewhere outside both
        // boxes the joined distance is negative (inside the shape), where the
        // plain union is positive. Search the corner's neighbourhood.
        let [a, b] = pill_and_drawer(16.0);
        let mut found = 0;
        let mut y = 80.0;
        while y <= 92.0 {
            let mut x = 284.0;
            while x <= 296.0 {
                let p = (x, y);
                let ua = box_arc(a.0, a.1, p).0;
                let ub = box_arc(b.0, b.1, p).0;
                let j = joined_distance(&[a, b], p);
                if ua > 0.0 && ub > 0.0 && j < 0.0 {
                    found += 1;
                }
                // The bite only ever lowers the union.
                assert!(j <= ua.min(ub) + 1e-4, "at {p:?}");
                x += 0.5;
            }
            y += 0.5;
        }
        assert!(found > 0, "no concave-corner point is inside the joined shape");
    }

    #[test]
    fn the_smooth_min_is_symmetric_at_one_fillet() {
        // The fillet belongs to the member joined in, so the order matters only
        // through it: with one fillet on both, swapping the members changes nothing.
        let [a, _] = pill_and_drawer(16.0);
        let b = ([290.0, 80.0, 50.0, 160.0], 12.0, 16.0);
        let a = (a.0, a.1, 16.0);
        for &p in &[(287.0, 83.0), (292.0, 86.0), (330.0, 200.0), (5.0, 60.0)] {
            let ab = joined_distance(&[a, b], p);
            let ba = joined_distance(&[b, a], p);
            assert!((ab - ba).abs() < 1e-4, "at {p:?}: {ab} vs {ba}");
        }
    }
}
