// SPDX-License-Identifier: AGPL-3.0-only
//! Coverage rasterisers for the round shapes the HUD is drawn with.
//!
//! The solid-colour element carries exactly a size and a colour, and the
//! bitmap font path is one-bit coverage replicated in integer blocks, so
//! neither can express a curve. This module draws on the CPU into the same
//! premultiplied-RGBA byte buffer [`super::text::Raster`] already uses, which
//! is uploaded as a texture -- so a curve costs nothing new at the boundary.
//!
//! Everything here is a signed distance field sampled once per pixel and put
//! through a one-pixel `smoothstep`, which is the same antialiasing the
//! rounded-window shader in [`super::effects`] does on the GPU. Geometry is in
//! device pixels: unlike the glyphs, curves want the full resolution.

use super::text::{premul, Rgba};

/// One pixel of antialiasing either side of the boundary.
fn coverage(d: f32) -> f32 {
    // `d` is negative inside the shape. Matches `effects::ROUNDED_TEX_SRC`.
    (0.5 - d).clamp(0.0, 1.0)
}

/// Source-over one pixel of `color` at `cov` coverage.
///
/// Out-of-bounds coordinates are dropped rather than wrapped: callers work in
/// shape space and are allowed to describe a shape that hangs off the buffer.
fn blend(px: &mut [u8], w: usize, h: usize, x: i32, y: i32, cov: f32, color: Rgba) {
    if cov <= 0.0 || x < 0 || y < 0 || x as usize >= w || y as usize >= h {
        return;
    }
    let cov = cov.min(1.0);
    let src = premul([color[0], color[1], color[2], color[3] * cov]);
    let i = (y as usize * w + x as usize) * 4;
    let inv = 1.0 - (src[3] as f32 / 255.0);
    for c in 0..4 {
        let dst = px[i + c] as f32 * inv;
        px[i + c] = (src[c] as f32 + dst).round().clamp(0.0, 255.0) as u8;
    }
}

/// Distance from `p` to the rounded rectangle `[0,0]..[w,h]` with corner
/// radius `r`. Negative inside.
fn round_rect_sdf(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    let qx = (x - w * 0.5).abs() - (w * 0.5 - r);
    let qy = (y - h * 0.5).abs() - (h * 0.5 - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// Fill the whole buffer's rounded-rect silhouette with `color`.
///
/// Used for the panel background, which is why it takes the buffer's own
/// dimensions rather than a sub-rectangle.
pub fn fill_round_rect(px: &mut [u8], w: usize, h: usize, radius: f32, color: Rgba) {
    for y in 0..h {
        for x in 0..w {
            let d = round_rect_sdf(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32, radius);
            blend(px, w, h, x as i32, y as i32, coverage(d), color);
        }
    }
}

/// Stroke the rounded-rect outline `thick` pixels wide, inset `inset` pixels
/// from the buffer edge.
pub fn stroke_round_rect(
    px: &mut [u8],
    w: usize,
    h: usize,
    inset: f32,
    radius: f32,
    thick: f32,
    color: Rgba,
) {
    let rw = w as f32 - inset * 2.0;
    let rh = h as f32 - inset * 2.0;
    if rw <= 0.0 || rh <= 0.0 {
        return;
    }
    for y in 0..h {
        for x in 0..w {
            let d = round_rect_sdf(x as f32 + 0.5 - inset, y as f32 + 0.5 - inset, rw, rh, radius);
            // Ring: distance to the outline itself.
            let ring = d.abs() - thick * 0.5;
            blend(px, w, h, x as i32, y as i32, coverage(ring), color);
        }
    }
}

/// Stroke the arc of radius `radius` about `(cx, cy)` between angles `a0` and
/// `a1` (radians, y down, `a1 > a0`).
#[allow(clippy::too_many_arguments)]
pub fn stroke_arc(
    px: &mut [u8],
    w: usize,
    h: usize,
    cx: f32,
    cy: f32,
    radius: f32,
    thick: f32,
    a0: f32,
    a1: f32,
    color: Rgba,
) {
    if radius <= 0.0 || thick <= 0.0 || a1 <= a0 {
        return;
    }
    let reach = radius + thick;
    let x0 = (cx - reach).floor() as i32;
    let x1 = (cx + reach).ceil() as i32;
    let y0 = (cy - reach).floor() as i32;
    let y1 = (cy + reach).ceil() as i32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let len = (dx * dx + dy * dy).sqrt();
            // Clamp the sample to the arc's angular span, so the ends are
            // round caps rather than a hard radial cut.
            let mut a = dy.atan2(dx);
            while a < a0 - std::f32::consts::PI {
                a += std::f32::consts::TAU;
            }
            let ca = a.clamp(a0, a1);
            let (nx, ny) = (cx + ca.cos() * radius, cy + ca.sin() * radius);
            let d = if a >= a0 && a <= a1 {
                (len - radius).abs()
            } else {
                let (ex, ey) = (x as f32 + 0.5 - nx, y as f32 + 0.5 - ny);
                (ex * ex + ey * ey).sqrt()
            };
            blend(px, w, h, x, y, coverage(d - thick * 0.5), color);
        }
    }
}

/// Stroke the capsule between two points -- the leader line, and the straight
/// arms of a corner bracket.
#[allow(clippy::too_many_arguments)]
pub fn stroke_line(
    px: &mut [u8],
    w: usize,
    h: usize,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    thick: f32,
    color: Rgba,
) {
    if thick <= 0.0 {
        return;
    }
    let reach = thick;
    let x0 = (ax.min(bx) - reach).floor().max(0.0) as i32;
    let x1 = (ax.max(bx) + reach).ceil().min(w as f32) as i32;
    let y0 = (ay.min(by) - reach).floor().max(0.0) as i32;
    let y1 = (ay.max(by) + reach).ceil().min(h as f32) as i32;
    let (ex, ey) = (bx - ax, by - ay);
    let len2 = (ex * ex + ey * ey).max(f32::EPSILON);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (pxx, pyy) = (x as f32 + 0.5 - ax, y as f32 + 0.5 - ay);
            let t = ((pxx * ex + pyy * ey) / len2).clamp(0.0, 1.0);
            let (dx, dy) = (pxx - ex * t, pyy - ey * t);
            let d = (dx * dx + dy * dy).sqrt() - thick * 0.5;
            blend(px, w, h, x, y, coverage(d), color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgba = [1.0, 1.0, 1.0, 1.0];

    fn alpha(px: &[u8], w: usize, x: usize, y: usize) -> u8 {
        px[(y * w + x) * 4 + 3]
    }

    #[test]
    fn a_rounded_fill_clips_its_corners_and_keeps_its_middle() {
        let (w, h) = (40, 40);
        let mut px = vec![0u8; w * h * 4];
        fill_round_rect(&mut px, w, h, 10.0, WHITE);
        assert_eq!(alpha(&px, w, 20, 20), 255);
        assert_eq!(alpha(&px, w, 0, 0), 0);
        assert_eq!(alpha(&px, w, w - 1, h - 1), 0);
        // The middle of an edge is still on the shape.
        assert_eq!(alpha(&px, w, 20, 0), 255);
    }

    #[test]
    fn a_stroke_covers_its_own_line_and_nothing_far_from_it() {
        let (w, h) = (40, 40);
        let mut px = vec![0u8; w * h * 4];
        stroke_line(&mut px, w, h, 5.0, 20.0, 35.0, 20.0, 3.0, WHITE);
        assert_eq!(alpha(&px, w, 20, 20), 255);
        assert_eq!(alpha(&px, w, 20, 30), 0);
        assert_eq!(alpha(&px, w, 0, 20), 0);
    }

    #[test]
    fn an_arc_stays_on_its_radius() {
        let (w, h) = (40, 40);
        let mut px = vec![0u8; w * h * 4];
        // Bottom-right quarter, centred at 20,20 with radius 12.
        stroke_arc(
            &mut px,
            w,
            h,
            20.0,
            20.0,
            12.0,
            3.0,
            0.0,
            std::f32::consts::FRAC_PI_2,
            WHITE,
        );
        assert_eq!(alpha(&px, w, 32, 20), 255);
        assert_eq!(alpha(&px, w, 20, 32), 255);
        // The opposite quadrant is not part of this arc.
        assert_eq!(alpha(&px, w, 8, 20), 0);
        // Nor is the centre.
        assert_eq!(alpha(&px, w, 20, 20), 0);
    }
}
