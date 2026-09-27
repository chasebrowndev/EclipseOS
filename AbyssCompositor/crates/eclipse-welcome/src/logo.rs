// SPDX-License-Identifier: AGPL-3.0-only
//! The logo, split in two and resampled to the exact size it is drawn at.
//!
//! **Why resample.** The asset is 796x144 and is drawn 50cqw wide: on a 1080p
//! output that is 960 px, on a 4K one 1920. Handed to the GPU as-is, it is
//! stretched by a bilinear filter, which turns every edge of a flat
//! white-and-gold wordmark into a two or three pixel ramp. That is the blur.
//! So it is resampled once, on the CPU, to the physical pixel size it will
//! occupy, with a Lanczos filter, and the drawn size is then exactly the
//! texture size (see `draw::logo_rect`), so the GPU's own filter has nothing
//! left to do. The wordmark's alpha is 0 or 255 except along its edges, so the
//! resampled edge is also steepened about its 0.5 contour, by the upscale
//! factor: it stays where the source put it and is about a pixel wide instead
//! of `factor` pixels.
//!
//! **Why two layers.** The asset's O is part of the picture, but the welcome
//! flies a real eclipse into that spot and the O must not exist until the
//! eclipse has landed on it. So the asset is taken apart:
//!
//! * `body`: every letter, with the O and its glow removed;
//! * `o`: the O ring and black disc, plus the glow around it.
//!
//! The glow is soft, radially symmetric and clean, so it is measured (a median
//! by radius, immune to the letters that overlap its tail) and subtracted from
//! the letters' alpha, and re-added to the O layer analytically, per output
//! pixel, so it is not smeared by the resample nor crushed by the edge
//! steepening. Drawn one over the other at the same rectangle the two layers
//! give back the asset.

use crate::draw::lit;
use iced::widget::image::Handle;
use image::imageops::{self, FilterType};
use image::{ImageBuffer, Rgba};

type Premul = ImageBuffer<Rgba<f32>, Vec<f32>>;

/// The asset's size, pixels.
pub const SRC_W: f32 = 796.0;
pub const SRC_H: f32 = 144.0;
/// The O's centre in asset pixels (its coverage-weighted centroid).
pub const O_CENTRE: (f32, f32) = (654.0, 74.0);
/// The O's outer diameter, asset pixels. This is the eclipse's `1em` when it
/// lands.
pub const O_DIAMETER: f32 = 110.0;
/// The black disc's diameter over the ring's (equal-area diameters, so
/// the antialiased rims count for what they cover). The eclipse's moon ends here.
pub const O_HOLE_RATIO: f32 = 78.0 / 110.0;
/// The glow's alpha reaches zero at about this radius from the O's centre.
const HALO_REACH: usize = 90;
/// Inside this radius a pixel belongs to the O, outside it to the letters.
/// The ring's edge is at 55; the letters start at 62.
const O_RADIUS: f32 = 57.0;

/// The O's box on a logo drawn at `rect` (any size): centre and outer diameter.
pub fn o_box(x: f32, y: f32, w: f32, h: f32) -> ((f32, f32), f32) {
    (
        (x + O_CENTRE.0 / SRC_W * w, y + O_CENTRE.1 / SRC_H * h),
        O_DIAMETER / SRC_W * w,
    )
}

/// The two layers, ready to draw one over the other at the same rectangle.
pub struct Layers {
    pub body: Handle,
    pub o: Handle,
}

impl Layers {
    /// The asset undivided, for a build whose decode failed (it is compiled in
    /// and tested, so this is a last resort): the O shows early, nothing panics.
    pub fn whole(png: &[u8]) -> Layers {
        Layers {
            body: Handle::from_bytes(png.to_vec()),
            o: Handle::from_rgba(1, 1, vec![0, 0, 0, 0]),
        }
    }
}

/// The decoded asset, taken apart.
pub struct LogoSource {
    /// The letters, premultiplied, glow subtracted.
    body: Premul,
    /// The O's ring and disc, premultiplied, without the glow.
    ring: Premul,
    /// The glow's alpha by radius, one entry per pixel of radius.
    halo: Vec<f32>,
    /// The ring's colour, straight, `0..1`: the glow is drawn in it.
    gold: [f32; 3],
    /// Height over width.
    pub aspect: f32,
}

fn dist_from_o(x: u32, y: u32) -> f32 {
    (x as f32 + 0.5 - O_CENTRE.0).hypot(y as f32 + 0.5 - O_CENTRE.1)
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(|a, b| a.total_cmp(b));
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// The glow's alpha at radius `r` (source pixels), interpolated between the
/// per-pixel table entries, which are centred on `i + 0.5`.
fn halo_at(halo: &[f32], r: f32) -> f32 {
    let r = r.max(O_RADIUS + 0.5);
    let f = r - 0.5;
    let i = f.floor() as usize;
    let k = f - i as f32;
    let a = halo.get(i).copied().unwrap_or(0.0);
    let b = halo.get(i + 1).copied().unwrap_or(0.0);
    a + (b - a) * k
}

impl LogoSource {
    pub fn decode(png: &[u8]) -> Option<LogoSource> {
        let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        if (w as f32, h as f32) != (SRC_W, SRC_H) {
            return None;
        }

        // The glow, by radius: the median alpha of every pixel at that radius.
        let mut bins: Vec<Vec<f32>> = vec![Vec::new(); HALO_REACH];
        let mut ring_sum = [0.0f32; 3];
        let mut ring_n = 0.0f32;
        for (x, y, p) in rgba.enumerate_pixels() {
            let r = dist_from_o(x, y);
            let a = f32::from(p.0[3]) / 255.0;
            if (r as usize) < HALO_REACH && r >= O_RADIUS {
                bins[r as usize].push(a);
            }
            // The solid ring: opaque and gold, between the disc and the edge.
            if (44.0..52.0).contains(&r) && p.0[3] == 255 {
                for (sum, &v) in ring_sum.iter_mut().zip(&p.0) {
                    *sum += f32::from(v) / 255.0;
                }
                ring_n += 1.0;
            }
        }
        let mut halo: Vec<f32> = bins.iter_mut().map(|b| median(b)).collect();
        // Inside the O's own radius there is no glow to speak of; hold the
        // first measured value so the analytic layer is continuous there.
        let first = halo[O_RADIUS as usize];
        for v in &mut halo[..O_RADIUS as usize] {
            *v = first;
        }
        let gold = if ring_n > 0.0 {
            ring_sum.map(|s| s / ring_n)
        } else {
            [1.0, 0.75, 0.24]
        };

        let mut body = Premul::new(w, h);
        let mut ring = Premul::new(w, h);
        for (x, y, p) in rgba.enumerate_pixels() {
            let a = f32::from(p.0[3]) / 255.0;
            let rgb = [0, 1, 2].map(|c| f32::from(p.0[c]) / 255.0);
            let r = dist_from_o(x, y);
            let g = halo_at(&halo, r);
            if r < O_RADIUS {
                // a = 1 - (1 - m)(1 - g)
                let m = ((a - g) / (1.0 - g)).clamp(0.0, 1.0);
                *ring.get_pixel_mut(x, y) = Rgba([rgb[0] * m, rgb[1] * m, rgb[2] * m, m]);
            } else {
                // The letters are what is left once the glow is taken from
                // under them. Anything the glow alone explains is not a letter.
                let l = (1.0 - (1.0 - a) / (1.0 - g)).clamp(0.0, 1.0);
                let l = if l < 0.03 { 0.0 } else { l };
                *body.get_pixel_mut(x, y) = Rgba([rgb[0] * l, rgb[1] * l, rgb[2] * l, l]);
            }
        }
        Some(LogoSource {
            body,
            ring,
            halo,
            gold,
            aspect: h as f32 / w as f32,
        })
    }

    /// The source's own pixels.
    pub fn native(&self) -> Layers {
        self.layers(SRC_W as u32, 1.0)
    }

    /// Resampled to `width` pixels wide, `round(width * aspect)` tall.
    pub fn sized(&self, width: u32) -> Layers {
        let gain = (width as f32 / SRC_W).max(1.0);
        self.layers(width, gain)
    }

    /// The height, in pixels, of a `width`-wide resample.
    pub fn height_for(&self, width: u32) -> u32 {
        ((width as f32 * self.aspect).round() as u32).max(1)
    }

    fn layers(&self, width: u32, gain: f32) -> Layers {
        let width = width.max(1);
        let height = self.height_for(width);
        Layers {
            body: self.render_body(width, height, gain),
            o: self.render_o(width, height, gain),
        }
    }

    fn resample(src: &Premul, width: u32, height: u32) -> Premul {
        if (width, height) == src.dimensions() {
            src.clone()
        } else {
            imageops::resize(src, width, height, FilterType::Lanczos3)
        }
    }

    /// Steepen an alpha about its 0.5 contour.
    fn steepen(a: f32, gain: f32) -> f32 {
        ((a.clamp(0.0, 1.0) - 0.5) * gain + 0.5).clamp(0.0, 1.0)
    }

    fn render_body(&self, width: u32, height: u32, gain: f32) -> Handle {
        let px = Self::resample(&self.body, width, height);
        let mut out = Vec::with_capacity(px.as_raw().len());
        for p in px.pixels() {
            let a = p.0[3].clamp(0.0, 1.0);
            let edge = Self::steepen(a, gain);
            for c in 0..3 {
                let straight = if a > 1e-3 {
                    (p.0[c] / a).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                out.push((straight * 255.0).round() as u8);
            }
            out.push((lit(edge) * 255.0).round() as u8);
        }
        Handle::from_rgba(width, height, out)
    }

    /// The O: the resampled ring and disc, with the glow laid under them,
    /// evaluated at each output pixel's own distance from the O's centre.
    fn render_o(&self, width: u32, height: u32, gain: f32) -> Handle {
        let px = Self::resample(&self.ring, width, height);
        let (sx, sy) = (SRC_W / width as f32, SRC_H / height as f32);
        let mut out = Vec::with_capacity(px.as_raw().len());
        for (x, y, p) in px.enumerate_pixels() {
            let m_raw = p.0[3].clamp(0.0, 1.0);
            let m = Self::steepen(m_raw, gain);
            let r = ((x as f32 + 0.5) * sx - O_CENTRE.0).hypot((y as f32 + 0.5) * sy - O_CENTRE.1);
            let g = if r < HALO_REACH as f32 {
                halo_at(&self.halo, r)
            } else {
                0.0
            };
            let a = m + (1.0 - m) * g;
            // Premultiplied colour of the ring over the glow.
            let mut straight = [0.0f32; 3];
            if a > 1e-4 {
                for ((out_c, &pc), &gold) in straight.iter_mut().zip(&p.0).zip(&self.gold) {
                    let ring = if m_raw > 1e-3 {
                        (pc / m_raw).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    *out_c = ((m * ring + (1.0 - m) * g * gold) / a).clamp(0.0, 1.0);
                }
            }
            for v in straight {
                out.push((v * 255.0).round() as u8);
            }
            // The glow is faint, and `lit` spends most of the 8 bits on the
            // bright end: rounding alone leaves visible rings. Dither it.
            out.push((lit(a) * 255.0 + dither(x, y)).floor().clamp(0.0, 255.0) as u8);
        }
        Handle::from_rgba(width, height, out)
    }
}

/// Interleaved gradient noise in `0..1`: a fixed, blue-ish pattern.
fn dither(x: u32, y: u32) -> f32 {
    let f = (0.067_110_56 * x as f32 + 0.005_837_15 * y as f32).fract();
    (52.982_918 * f).fract()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = include_bytes!("../assets/eclipseos-logo.png");

    fn logo() -> LogoSource {
        LogoSource::decode(PNG).unwrap()
    }

    #[test]
    fn the_asset_decodes_with_its_aspect() {
        assert!((logo().aspect - SRC_H / SRC_W).abs() < 1e-6);
    }

    #[test]
    fn a_resample_is_the_size_asked_for() {
        let logo = logo();
        assert_eq!(logo.height_for(796), 144);
        assert_eq!(logo.height_for(1920), 347);
        assert_eq!(logo.height_for(1), 1);
    }

    /// The constants describe the asset: the ring's alpha edge is where the
    /// diameter says, and the black disc is where the hole ratio says.
    #[test]
    fn the_o_constants_match_the_asset() {
        let rgba = image::load_from_memory(PNG).unwrap().to_rgba8();
        let (cx, cy) = (O_CENTRE.0 as u32, O_CENTRE.1 as u32);
        let row = |x: u32| rgba.get_pixel(x, cy - 1).0;
        // Ring: opaque gold just inside the radius, glow only just outside.
        let inside = row(cx - (O_DIAMETER / 2.0) as u32 + 2);
        assert_eq!(inside[3], 255);
        assert!(inside[0] > 200 && inside[2] < 100);
        let outside = row(cx - (O_DIAMETER / 2.0) as u32 - 3);
        assert!(outside[3] < 90, "glow, not ring: {outside:?}");
        // Disc: opaque black just inside its radius, gold ring just outside.
        let hole = O_HOLE_RATIO * O_DIAMETER / 2.0;
        let disc = row(cx - hole as u32 + 2);
        assert_eq!((disc[0], disc[3]), (0, 255));
        let gold = row(cx - hole as u32 - 2);
        assert!(gold[0] > 200);
    }

    #[test]
    fn the_glow_is_measured_and_falls_off() {
        let logo = logo();
        let at = |r: f32| halo_at(&logo.halo, r);
        assert!(at(58.0) > 0.12 && at(58.0) < 0.22, "{}", at(58.0));
        assert!(at(58.0) > at(70.0) && at(70.0) > at(82.0));
        assert!(at(82.0) < 0.03);
        assert!(at(200.0) == 0.0);
    }

    /// Nothing of the O is left in the letters: the ring, the disc and the
    /// glow around them are all gone.
    #[test]
    fn the_body_has_no_o_in_it() {
        let logo = logo();
        for (x, y, p) in logo.body.enumerate_pixels() {
            if dist_from_o(x, y) < O_RADIUS {
                assert_eq!(p.0[3], 0.0, "ring pixel ({x},{y}) survived");
            }
        }
        // The glow's own pixels (clear of any letter) are gone too. Sample a
        // ring of them straight above the O.
        for x in 640..668 {
            for y in 0..12 {
                let a = logo.body.get_pixel(x, y).0[3];
                assert!(a < 0.05, "glow left at ({x},{y}): {a}");
            }
        }
        // The letters are still there: the S is opaque somewhere.
        assert!(logo.body.pixels().any(|p| p.0[3] > 0.99));
    }

    /// The two layers are the asset: over a black ground, body then O gives
    /// back the original alpha everywhere (within the 8-bit quantisation of
    /// the glow).
    #[test]
    fn the_layers_recompose_to_the_asset() {
        let logo = logo();
        let rgba = image::load_from_memory(PNG).unwrap().to_rgba8();
        for (x, y, p) in rgba.enumerate_pixels() {
            let want = f32::from(p.0[3]) / 255.0;
            let l = logo.body.get_pixel(x, y).0[3];
            let o = logo.ring.get_pixel(x, y).0[3];
            let r = dist_from_o(x, y);
            let g = halo_at(&logo.halo, r);
            let o_layer = o + (1.0 - o) * g;
            let got = 1.0 - (1.0 - l) * (1.0 - o_layer);
            assert!((got - want).abs() < 0.05, "({x},{y}): {got} vs {want}");
        }
    }

    #[test]
    fn the_o_box_scales_with_the_logo() {
        let (c, d) = o_box(100.0, 50.0, 796.0, 144.0);
        assert!((c.0 - 754.0).abs() < 1e-3 && (c.1 - 124.0).abs() < 1e-3);
        assert!((d - 110.0).abs() < 1e-3);
        let (c, d) = o_box(0.0, 0.0, 1592.0, 288.0);
        assert!((c.0 - 1308.0).abs() < 1e-3 && (d - 220.0).abs() < 1e-3);
    }
}
