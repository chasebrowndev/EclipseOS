// SPDX-License-Identifier: AGPL-3.0-only
//! COLRv0/v1 painter (T-06 §7). skrifa walks the paint graph and calls [`ColorPainter`]; we
//! turn every clip and glyph path into a `zeno` coverage mask and fill solid, linear, radial and
//! sweep brushes onto premultiplied layers merged with the COLRv1 composite modes. The result is
//! premultiplied RGBA8 for the colour atlas. OT-SVG is not supported.

use crate::face::FaceInstance;
use crate::raster::ColorBitmap;
use skrifa::{
    MetadataProvider,
    color::{Brush, ColorPainter, ColorStop, CompositeMode, Extend, Transform},
    instance::{Location, Size},
    outline::{DrawSettings, OutlineGlyphCollection, OutlinePen},
    raw::{TableProvider, types::BoundingBox, types::GlyphId},
};
use zeno::{Command, Mask, Vector};

/// Largest pixel size we paint at (the canvas is two ems square).
const MAX_PX: f32 = 512.0;
/// Pixel-operation budget per glyph; bounds the time a hostile paint graph can take.
const BUDGET: u64 = 64_000_000;
/// Cap on simultaneously live layers and clip masks: bounds memory (the budget bounds time).
const MAX_LIVE: usize = 8;
/// Palette index meaning "the text foreground colour".
const FOREGROUND: u16 = 0xFFFF;

type Rgba = [f32; 4];

struct Layer {
    px: Vec<Rgba>,
    mode: CompositeMode,
}

struct Painter<'a> {
    outlines: OutlineGlyphCollection<'a>,
    location: &'a Location,
    palette: Vec<Rgba>,
    foreground: Rgba,
    w: usize,
    h: usize,
    ctm: Vec<Transform>,
    clips: Vec<Vec<f32>>,
    layers: Vec<Layer>,
    spent: u64,
    exhausted: bool,
}

struct PathPen {
    cmds: Vec<Command>,
    t: Transform,
}

impl PathPen {
    fn map(&self, x: f32, y: f32) -> Vector {
        let t = &self.t;
        Vector::new(t.xx * x + t.xy * y + t.dx, t.yx * x + t.yy * y + t.dy)
    }
}

impl OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.cmds.push(Command::MoveTo(p));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.cmds.push(Command::LineTo(p));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.map(cx0, cy0), self.map(x, y));
        self.cmds.push(Command::QuadTo(c, p));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (a, b, p) = (self.map(cx0, cy0), self.map(cx1, cy1), self.map(x, y));
        self.cmds.push(Command::CurveTo(a, b, p));
    }
    fn close(&mut self) {
        self.cmds.push(Command::Close);
    }
}

fn invert(t: &Transform) -> Option<Transform> {
    let det = t.xx * t.yy - t.xy * t.yx;
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let xx = t.yy / det;
    let xy = -t.xy / det;
    let yx = -t.yx / det;
    let yy = t.xx / det;
    Some(Transform {
        xx,
        yx,
        xy,
        yy,
        dx: -(xx * t.dx + xy * t.dy),
        dy: -(yx * t.dx + yy * t.dy),
    })
}

fn apply(t: &Transform, x: f32, y: f32) -> (f32, f32) {
    (t.xx * x + t.xy * y + t.dx, t.yx * x + t.yy * y + t.dy)
}

fn premul(c: Rgba) -> Rgba {
    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
}

// ---------------------------------------------------------------- compositing

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut c = c;
    if n < 0.0 && l - n != 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 && x - l != 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap_or(std::cmp::Ordering::Equal));
    let (mn, md, mx) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0; 3];
    if c[mx] > c[mn] {
        out[md] = (c[md] - c[mn]) * s / (c[mx] - c[mn]);
        out[mx] = s;
    }
    out
}

fn blend_channel(mode: CompositeMode, cb: f32, cs: f32) -> f32 {
    use CompositeMode::*;
    fn hard_light(cb: f32, cs: f32) -> f32 {
        if cs <= 0.5 {
            cb * 2.0 * cs
        } else {
            let s = 2.0 * cs - 1.0;
            cb + s - cb * s
        }
    }
    match mode {
        Multiply => cb * cs,
        Screen => cb + cs - cb * cs,
        Overlay => hard_light(cs, cb),
        Darken => cb.min(cs),
        Lighten => cb.max(cs),
        ColorDodge => {
            if cb == 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        HardLight => hard_light(cb, cs),
        SoftLight => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16.0 * cb - 12.0) * cb + 4.0) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        Difference => (cb - cs).abs(),
        Exclusion => cb + cs - 2.0 * cb * cs,
        _ => cs,
    }
}

/// Composites premultiplied `src` over premultiplied `dst` with `mode`.
pub(crate) fn composite(src: Rgba, dst: Rgba, mode: CompositeMode) -> Rgba {
    use CompositeMode::*;
    let (a_s, a_b) = (src[3], dst[3]);
    let pd = |fa: f32, fb: f32| -> Rgba {
        std::array::from_fn(|i| (src[i] * fa + dst[i] * fb).clamp(0.0, 1.0))
    };
    match mode {
        Clear => [0.0; 4],
        Src => src,
        Dest => dst,
        SrcOver | Unknown => pd(1.0, 1.0 - a_s),
        DestOver => pd(1.0 - a_b, 1.0),
        SrcIn => pd(a_b, 0.0),
        DestIn => pd(0.0, a_s),
        SrcOut => pd(1.0 - a_b, 0.0),
        DestOut => pd(0.0, 1.0 - a_s),
        SrcAtop => pd(a_b, 1.0 - a_s),
        DestAtop => pd(1.0 - a_b, a_s),
        Xor => pd(1.0 - a_b, 1.0 - a_s),
        Plus => pd(1.0, 1.0),
        _ => {
            let ao = a_s + a_b - a_s * a_b;
            if a_s <= 0.0 || a_b <= 0.0 {
                return pd(1.0, 1.0 - a_s);
            }
            let cs = [src[0] / a_s, src[1] / a_s, src[2] / a_s];
            let cb = [dst[0] / a_b, dst[1] / a_b, dst[2] / a_b];
            let blended = match mode {
                HslHue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
                HslSaturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
                HslColor => set_lum(cs, lum(cb)),
                HslLuminosity => set_lum(cb, lum(cs)),
                m => [
                    blend_channel(m, cb[0], cs[0]),
                    blend_channel(m, cb[1], cs[1]),
                    blend_channel(m, cb[2], cs[2]),
                ],
            };
            std::array::from_fn(|i| {
                if i == 3 {
                    ao.clamp(0.0, 1.0)
                } else {
                    (src[i] * (1.0 - a_b) + dst[i] * (1.0 - a_s) + a_s * a_b * blended[i])
                        .clamp(0.0, 1.0)
                }
            })
        }
    }
}

// ------------------------------------------------------------------ gradients

fn extend_t(t: f32, e: Extend) -> f32 {
    match e {
        Extend::Repeat => t - t.floor(),
        Extend::Reflect => {
            let m = t.rem_euclid(2.0);
            if m > 1.0 { 2.0 - m } else { m }
        }
        _ => t.clamp(0.0, 1.0),
    }
}

/// Premultiplied colour at `t` (already extended into 0..=1), interpolated premultiplied.
fn stop_color(stops: &[(f32, Rgba)], t: f32) -> Rgba {
    let Some(first) = stops.first() else {
        return [0.0; 4];
    };
    if t <= first.0 {
        return first.1;
    }
    for pair in stops.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if t <= b.0 {
            let span = b.0 - a.0;
            if span <= 0.0 {
                return b.1;
            }
            let k = (t - a.0) / span;
            return std::array::from_fn(|i| a.1[i] + (b.1[i] - a.1[i]) * k);
        }
    }
    stops.last().map_or([0.0; 4], |s| s.1)
}

enum Prepared {
    Solid(Rgba),
    Linear {
        p0: (f32, f32),
        d: (f32, f32),
        inv_len2: f32,
        stops: Vec<(f32, Rgba)>,
        extend: Extend,
    },
    Radial {
        c0: (f32, f32),
        cd: (f32, f32),
        r0: f32,
        dr: f32,
        stops: Vec<(f32, Rgba)>,
        extend: Extend,
    },
    Sweep {
        c0: (f32, f32),
        start: f32,
        span: f32,
        stops: Vec<(f32, Rgba)>,
        extend: Extend,
    },
}

impl Prepared {
    /// Premultiplied colour at a point in font units; `None` where the gradient paints nothing.
    fn eval(&self, x: f32, y: f32) -> Option<Rgba> {
        match self {
            Prepared::Solid(c) => Some(*c),
            Prepared::Linear {
                p0,
                d,
                inv_len2,
                stops,
                extend,
            } => {
                let t = ((x - p0.0) * d.0 + (y - p0.1) * d.1) * inv_len2;
                Some(stop_color(stops, extend_t(t, *extend)))
            }
            Prepared::Radial {
                c0,
                cd,
                r0,
                dr,
                stops,
                extend,
            } => {
                let (px, py) = (x - c0.0, y - c0.1);
                let a = cd.0 * cd.0 + cd.1 * cd.1 - dr * dr;
                let b = px * cd.0 + py * cd.1 + r0 * dr;
                let c = px * px + py * py - r0 * r0;
                let t = if a.abs() < 1e-9 {
                    if b.abs() < 1e-12 {
                        return None;
                    }
                    let t = c / (2.0 * b);
                    if r0 + t * dr < 0.0 {
                        return None;
                    }
                    t
                } else {
                    let disc = b * b - a * c;
                    if disc < 0.0 {
                        return None;
                    }
                    let s = disc.sqrt();
                    let (t1, t2) = ((b + s) / a, (b - s) / a);
                    let (hi, lo) = if t1 >= t2 { (t1, t2) } else { (t2, t1) };
                    if r0 + hi * dr >= 0.0 {
                        hi
                    } else if r0 + lo * dr >= 0.0 {
                        lo
                    } else {
                        return None;
                    }
                };
                Some(stop_color(stops, extend_t(t, *extend)))
            }
            Prepared::Sweep {
                c0,
                start,
                span,
                stops,
                extend,
            } => {
                let (dx, dy) = (x - c0.0, y - c0.1);
                // Font space is y-up, so atan2 is counter-clockwise; skrifa wants clockwise.
                let ccw = dy.atan2(dx).to_degrees().rem_euclid(360.0);
                let cw = (360.0 - ccw).rem_euclid(360.0);
                if span.abs() < 1e-9 {
                    return Some(stop_color(stops, if cw >= *start { 1.0 } else { 0.0 }));
                }
                Some(stop_color(stops, extend_t((cw - start) / span, *extend)))
            }
        }
    }
}

impl Painter<'_> {
    fn spend(&mut self, n: usize) -> bool {
        self.spent += n as u64;
        if self.spent > BUDGET {
            self.exhausted = true;
        }
        !self.exhausted
    }

    fn color_of(&self, idx: u16, alpha: f32) -> Rgba {
        let mut c = if idx == FOREGROUND {
            self.foreground
        } else {
            self.palette
                .get(usize::from(idx))
                .copied()
                .unwrap_or([0.0; 4])
        };
        c[3] = (c[3] * alpha).clamp(0.0, 1.0);
        premul(c)
    }

    fn stops(&self, cs: &[ColorStop]) -> Vec<(f32, Rgba)> {
        let mut v: Vec<(f32, Rgba)> = cs
            .iter()
            .map(|s| (s.offset, self.color_of(s.palette_index, s.alpha)))
            .collect();
        v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    fn prepare(&self, brush: Brush<'_>) -> Option<Prepared> {
        Some(match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => Prepared::Solid(self.color_of(palette_index, alpha)),
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => {
                let d = (p1.x - p0.x, p1.y - p0.y);
                let len2 = d.0 * d.0 + d.1 * d.1;
                if len2 <= 0.0 || !len2.is_finite() {
                    return None;
                }
                Prepared::Linear {
                    p0: (p0.x, p0.y),
                    d,
                    inv_len2: 1.0 / len2,
                    stops: self.stops(color_stops),
                    extend,
                }
            }
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => Prepared::Radial {
                c0: (c0.x, c0.y),
                cd: (c1.x - c0.x, c1.y - c0.y),
                r0,
                dr: r1 - r0,
                stops: self.stops(color_stops),
                extend,
            },
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => Prepared::Sweep {
                c0: (c0.x, c0.y),
                start: start_angle,
                span: end_angle - start_angle,
                stops: self.stops(color_stops),
                extend,
            },
        })
    }

    fn current_ctm(&self) -> Transform {
        self.ctm.last().copied().unwrap_or(Transform {
            xx: 1.0,
            yx: 0.0,
            xy: 0.0,
            yy: 1.0,
            dx: 0.0,
            dy: 0.0,
        })
    }

    /// Rasterises `cmds` (pixel space) into a coverage mask multiplied by the current clip.
    fn mask_of(&mut self, cmds: &[Command]) -> Option<Vec<f32>> {
        let n = self.w * self.h;
        if self.clips.len() >= MAX_LIVE {
            self.exhausted = true;
        }
        if !self.spend(n) {
            return None;
        }
        if cmds.is_empty() {
            return Some(vec![0.0; n]);
        }
        let mut buf = vec![0u8; n];
        let v = cmds.to_vec();
        Mask::new(&v)
            .size(self.w as u32, self.h as u32)
            .render_into(&mut buf, None);
        Some(match self.clips.last() {
            Some(parent) => buf
                .iter()
                .zip(parent)
                .map(|(&b, &p)| f32::from(b) / 255.0 * p)
                .collect(),
            None => buf.iter().map(|&b| f32::from(b) / 255.0).collect(),
        })
    }
}

impl ColorPainter for Painter<'_> {
    fn push_transform(&mut self, transform: Transform) {
        let t = self.current_ctm() * transform;
        self.ctm.push(t);
    }

    fn pop_transform(&mut self) {
        if self.ctm.len() > 1 {
            self.ctm.pop();
        }
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        let mut pen = PathPen {
            cmds: Vec::new(),
            t: self.current_ctm(),
        };
        if let Some(g) = self.outlines.get(glyph_id) {
            let _ = g.draw(
                DrawSettings::unhinted(Size::unscaled(), self.location),
                &mut pen,
            );
        }
        // On exhaustion nothing is pushed: the whole result is discarded.
        if let Some(mask) = self.mask_of(&pen.cmds) {
            self.clips.push(mask);
        }
    }

    fn push_clip_box(&mut self, b: BoundingBox<f32>) {
        let t = self.current_ctm();
        let pt = |x: f32, y: f32| {
            let (x, y) = apply(&t, x, y);
            Vector::new(x, y)
        };
        let cmds = [
            Command::MoveTo(pt(b.x_min, b.y_min)),
            Command::LineTo(pt(b.x_max, b.y_min)),
            Command::LineTo(pt(b.x_max, b.y_max)),
            Command::LineTo(pt(b.x_min, b.y_max)),
            Command::Close,
        ];
        if let Some(mask) = self.mask_of(&cmds) {
            self.clips.push(mask);
        }
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn fill(&mut self, brush: Brush<'_>) {
        let n = self.w * self.h;
        if !self.spend(n) {
            return;
        }
        let Some(prep) = self.prepare(brush) else {
            return;
        };
        let Some(inv) = invert(&self.current_ctm()) else {
            return;
        };
        let (w, h) = (self.w, self.h);
        let Self { clips, layers, .. } = self;
        let Some(layer) = layers.last_mut() else {
            return;
        };
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let cov = clips.last().map_or(1.0, |m| m[i]);
                if cov <= 0.0 {
                    continue;
                }
                let (fx, fy) = apply(&inv, x as f32 + 0.5, y as f32 + 0.5);
                let Some(c) = prep.eval(fx, fy) else { continue };
                let s = [c[0] * cov, c[1] * cov, c[2] * cov, c[3] * cov];
                layer.px[i] = composite(s, layer.px[i], CompositeMode::SrcOver);
            }
        }
    }

    fn push_layer(&mut self, mode: CompositeMode) {
        let n = self.w * self.h;
        if self.layers.len() >= MAX_LIVE {
            self.exhausted = true;
        }
        if !self.spend(n) {
            return;
        }
        self.layers.push(Layer {
            px: vec![[0.0; 4]; n],
            mode,
        });
    }

    fn pop_layer_with_mode(&mut self, _mode: CompositeMode) {
        if self.layers.len() < 2 {
            return;
        }
        let Some(top) = self.layers.pop() else { return };
        let n = self.w * self.h;
        if !self.spend(n) {
            return;
        }
        let Self { clips, layers, .. } = self;
        let Some(dst) = layers.last_mut() else { return };
        for i in 0..n {
            let cov = clips.last().map_or(1.0, |m| m[i]);
            let out = composite(top.px[i], dst.px[i], top.mode);
            let d = dst.px[i];
            dst.px[i] = if cov >= 1.0 {
                out
            } else {
                [
                    d[0] + (out[0] - d[0]) * cov,
                    d[1] + (out[1] - d[1]) * cov,
                    d[2] + (out[2] - d[2]) * cov,
                    d[3] + (out[3] - d[3]) * cov,
                ]
            };
        }
    }
}

/// The CPAL palette 0 as straight RGBA.
fn palette0(font: &skrifa::FontRef<'_>) -> Vec<Rgba> {
    let Ok(cpal) = font.cpal() else {
        return Vec::new();
    };
    let Some(first) = cpal
        .color_record_indices()
        .first()
        .map(|i| usize::from(i.get()))
    else {
        return Vec::new();
    };
    let Some(Ok(records)) = cpal.color_records_array() else {
        return Vec::new();
    };
    (0..usize::from(cpal.num_palette_entries()))
        .filter_map(|i| records.get(first + i))
        .map(|r| {
            [
                f32::from(r.red()) / 255.0,
                f32::from(r.green()) / 255.0,
                f32::from(r.blue()) / 255.0,
                f32::from(r.alpha()) / 255.0,
            ]
        })
        .collect()
}

/// Paints `glyph` of `inst` at `px_size` pixels per em. `foreground` is straight RGBA for palette
/// entry 0xFFFF. `None` if the glyph has no COLR paint, painting fails, or the budget is spent.
pub(crate) fn paint_glyph(
    inst: &FaceInstance,
    glyph: u32,
    px_size: f32,
    foreground: [u8; 4],
) -> Option<ColorBitmap> {
    if !(1.0..=MAX_PX).contains(&px_size) {
        return None;
    }
    let face = inst.face();
    let font = face.font()?;
    let upem = f32::from(face.units_per_em());
    let cg = font.color_glyphs().get(GlyphId::new(glyph))?;
    let s = px_size / upem;
    let ox = (px_size * 0.5).ceil();
    let oy = (px_size * 1.5).ceil();
    let w = (ox + (px_size * 1.5).ceil()) as usize;
    let h = (oy + (px_size * 0.5).ceil()) as usize;
    let n = w * h;
    let base = Transform {
        xx: s,
        yx: 0.0,
        xy: 0.0,
        yy: -s,
        dx: ox,
        dy: oy,
    };
    let mut p = Painter {
        outlines: font.outline_glyphs(),
        location: inst.location(),
        palette: palette0(&font),
        foreground: [
            f32::from(foreground[0]) / 255.0,
            f32::from(foreground[1]) / 255.0,
            f32::from(foreground[2]) / 255.0,
            f32::from(foreground[3]) / 255.0,
        ],
        w,
        h,
        ctm: vec![base],
        clips: Vec::new(),
        layers: vec![Layer {
            px: vec![[0.0; 4]; n],
            mode: CompositeMode::SrcOver,
        }],
        spent: 0,
        exhausted: false,
    };
    cg.paint(inst.location(), &mut p).ok()?;
    if p.exhausted || p.layers.len() != 1 {
        return None;
    }
    let root = p.layers.pop()?;
    let q = |v: f32| (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if q(root.px[y * w + x][3]) != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (bw, bh) = (x1 - x0, y1 - y0);
    let mut data = Vec::with_capacity(bw * bh * 4);
    for y in y0..y1 {
        for x in x0..x1 {
            let c = root.px[y * w + x];
            let a = q(c[3]);
            // Keep premultiplied channels <= alpha after rounding.
            data.extend_from_slice(&[q(c[0]).min(a), q(c[1]).min(a), q(c[2]).min(a), a]);
        }
    }
    Some(ColorBitmap {
        left: x0 as i32 - ox as i32,
        top: oy as i32 - y0 as i32,
        width: bw as u32,
        height: bh as u32,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Rgba, b: Rgba) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    const RED: Rgba = [1.0, 0.0, 0.0, 1.0];
    const BLUE_HALF: Rgba = [0.0, 0.0, 0.5, 0.5];

    #[test]
    fn porter_duff_modes() {
        let (s, d) = (BLUE_HALF, RED);
        assert!(close(composite(s, d, CompositeMode::Clear), [0.0; 4]));
        assert!(close(composite(s, d, CompositeMode::Src), s));
        assert!(close(composite(s, d, CompositeMode::Dest), d));
        assert!(close(
            composite(s, d, CompositeMode::SrcOver),
            [0.5, 0.0, 0.5, 1.0]
        ));
        assert!(close(composite(s, d, CompositeMode::DestOver), d));
        assert!(close(composite(s, d, CompositeMode::SrcIn), s));
        assert!(close(
            composite(s, d, CompositeMode::DestIn),
            [0.5, 0.0, 0.0, 0.5]
        ));
        assert!(close(composite(s, d, CompositeMode::SrcOut), [0.0; 4]));
        assert!(close(
            composite(s, d, CompositeMode::DestOut),
            [0.5, 0.0, 0.0, 0.5]
        ));
        assert!(close(
            composite(s, d, CompositeMode::SrcAtop),
            [0.5, 0.0, 0.5, 1.0]
        ));
        assert!(close(
            composite(s, d, CompositeMode::DestAtop),
            [0.5, 0.0, 0.0, 0.5]
        ));
        assert!(close(
            composite(s, d, CompositeMode::Xor),
            [0.5, 0.0, 0.0, 0.5]
        ));
        assert!(close(
            composite(s, d, CompositeMode::Plus),
            [1.0, 0.0, 0.5, 1.0]
        ));
    }

    #[test]
    fn separable_blend_modes_on_opaque() {
        let a = [0.5, 0.25, 1.0, 1.0];
        let b = [0.5, 0.5, 0.5, 1.0];
        assert!(close(
            composite(a, b, CompositeMode::Multiply),
            [0.25, 0.125, 0.5, 1.0]
        ));
        assert!(close(
            composite(a, b, CompositeMode::Screen),
            [0.75, 0.625, 1.0, 1.0]
        ));
        assert!(close(
            composite(a, b, CompositeMode::Darken),
            [0.5, 0.25, 0.5, 1.0]
        ));
        assert!(close(
            composite(a, b, CompositeMode::Lighten),
            [0.5, 0.5, 1.0, 1.0]
        ));
        assert!(close(
            composite(a, b, CompositeMode::Difference),
            [0.0, 0.25, 0.5, 1.0]
        ));
        assert!(close(
            composite(a, b, CompositeMode::Exclusion),
            [0.5, 0.5, 0.5, 1.0]
        ));
        // Overlay(cb, cs) is hard-light with the roles swapped.
        assert!(close(
            composite(a, b, CompositeMode::Overlay),
            composite(b, a, CompositeMode::HardLight)
        ));
        for m in [
            CompositeMode::ColorDodge,
            CompositeMode::ColorBurn,
            CompositeMode::SoftLight,
            CompositeMode::HslHue,
            CompositeMode::HslSaturation,
            CompositeMode::HslColor,
            CompositeMode::HslLuminosity,
        ] {
            let o = composite(a, b, m);
            assert!(
                o.iter().all(|v| (0.0..=1.0).contains(v)),
                "{m:?} out of range: {o:?}"
            );
            assert!((o[3] - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn luminosity_keeps_backdrop_hue_and_source_luma() {
        let src = [1.0, 1.0, 1.0, 1.0];
        let dst = [1.0, 0.0, 0.0, 1.0];
        let o = composite(src, dst, CompositeMode::HslLuminosity);
        assert!((lum([o[0], o[1], o[2]]) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn extend_modes() {
        assert_eq!(extend_t(-0.5, Extend::Pad), 0.0);
        assert_eq!(extend_t(1.5, Extend::Pad), 1.0);
        assert!((extend_t(1.25, Extend::Repeat) - 0.25).abs() < 1e-6);
        assert!((extend_t(-0.25, Extend::Repeat) - 0.75).abs() < 1e-6);
        assert!((extend_t(1.25, Extend::Reflect) - 0.75).abs() < 1e-6);
        assert!((extend_t(2.25, Extend::Reflect) - 0.25).abs() < 1e-6);
    }

    fn two_stops() -> Vec<(f32, Rgba)> {
        vec![(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 1.0, 1.0])]
    }

    #[test]
    fn linear_gradient_projects_onto_the_axis() {
        let g = Prepared::Linear {
            p0: (0.0, 0.0),
            d: (10.0, 0.0),
            inv_len2: 0.01,
            stops: two_stops(),
            extend: Extend::Pad,
        };
        assert!(close(g.unwrap_at(5.0, 99.0), [0.5, 0.5, 0.5, 1.0]));
        assert!(close(g.unwrap_at(-5.0, 0.0), [0.0, 0.0, 0.0, 1.0]));
        assert!(close(g.unwrap_at(50.0, 0.0), [1.0; 4]));
    }

    #[test]
    fn radial_gradient_concentric() {
        let g = Prepared::Radial {
            c0: (0.0, 0.0),
            cd: (0.0, 0.0),
            r0: 0.0,
            dr: 10.0,
            stops: two_stops(),
            extend: Extend::Pad,
        };
        assert!(close(g.unwrap_at(5.0, 0.0), [0.5, 0.5, 0.5, 1.0]));
        assert!(close(g.unwrap_at(0.0, -10.0), [1.0; 4]));
        assert!(close(g.unwrap_at(0.0, 0.0), [0.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn sweep_gradient_is_clockwise_from_positive_x() {
        let g = Prepared::Sweep {
            c0: (0.0, 0.0),
            start: 0.0,
            span: 360.0,
            stops: two_stops(),
            extend: Extend::Pad,
        };
        // Font space is y-up: a point straight down is a quarter turn clockwise.
        assert!(close(g.unwrap_at(0.0, -1.0), [0.25, 0.25, 0.25, 1.0]));
        // Straight up is three quarters clockwise.
        assert!(close(g.unwrap_at(0.0, 1.0), [0.75, 0.75, 0.75, 1.0]));
    }

    #[test]
    fn transform_inverse_roundtrips() {
        let t = Transform {
            xx: 2.0,
            yx: 0.5,
            xy: -1.0,
            yy: 3.0,
            dx: 4.0,
            dy: -2.0,
        };
        let inv = invert(&t).unwrap();
        let (x, y) = apply(&t, 1.5, -2.5);
        let (bx, by) = apply(&inv, x, y);
        assert!((bx - 1.5).abs() < 1e-4 && (by + 2.5).abs() < 1e-4);
        let singular = Transform {
            xx: 1.0,
            yx: 2.0,
            xy: 2.0,
            yy: 4.0,
            dx: 0.0,
            dy: 0.0,
        };
        assert!(invert(&singular).is_none());
    }

    impl Prepared {
        fn unwrap_at(&self, x: f32, y: f32) -> Rgba {
            self.eval(x, y).expect("gradient paints here")
        }
    }
}
