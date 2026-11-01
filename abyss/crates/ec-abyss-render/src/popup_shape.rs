// SPDX-License-Identifier: AGPL-3.0-only
//! The rounded shape of a keyed window's popup (ADR 0080), read off the
//! popup's own texture alpha, whatever buffer type it came from.
//!
//! A Firefox menu or panel is drawn rounded with transparent corner pixels, but
//! the blur behind a popup is a plain rectangle. Rather than hard-code a
//! radius, the radius is estimated from how much of each corner the client left
//! transparent: a quarter circle of radius `r` leaves `r^2 (1 - pi/4)` pixels of
//! corner uncovered. That works for any radius and any anti-aliasing, and costs
//! one readback of four small corner blocks when the buffer size changes.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::{
    gles::{GlesRenderer, GlesTexture},
    utils::with_renderer_surface_state,
    ExportMem, Renderer, Texture,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Rectangle};
use smithay::wayland::compositor::with_states;
use std::sync::Mutex;

/// Corner block scanned, buffer px. Radii past this read as this.
const SCAN: i32 = 32;
/// `1 - pi/4`: the share of a `r x r` corner square a quarter circle leaves out.
const CORNER_MISS: f32 = 0.214_601_84;
/// Below this the corner is square for all practical purposes.
const MIN_RADIUS: f32 = 1.5;

/// Corner radius, in buffer pixels, of a `w x h` image whose alpha (0..=255)
/// at `(x, y)` is `alpha(x, y)`. The largest of the four corners, so a menu
/// that is square on one side still gets its rounded side. `0.0` when square.
pub fn corner_radius(w: i32, h: i32, alpha: impl Fn(i32, i32) -> u8) -> f32 {
    let n = SCAN.min(w / 2).min(h / 2);
    if n < 2 {
        return 0.0;
    }
    let mut best = 0.0f32;
    for (ox, oy, sx, sy) in [
        (0, 0, 1, 1),
        (w - 1, 0, -1, 1),
        (0, h - 1, 1, -1),
        (w - 1, h - 1, -1, -1),
    ] {
        let mut miss = 0.0f32;
        for j in 0..n {
            for i in 0..n {
                miss += 1.0 - alpha(ox + sx * i, oy + sy * j) as f32 / 255.0;
            }
        }
        best = best.max((miss / CORNER_MISS).sqrt());
    }
    if best < MIN_RADIUS {
        0.0
    } else {
        best.min(n as f32)
    }
}

/// The cached estimate: the buffer size it was read at and the buffer-pixel
/// radius. Corners do not change as a menu's content does, so a commit that
/// keeps the size keeps the radius; only a new size reads the texture again.
struct Cached(Mutex<Option<((i32, i32), f32)>>);

/// The logical corner radius of `surface`'s popup shape, or `None` when its
/// texture cannot be read back. Read off the imported texture (shm and dmabuf
/// alike), so any buffer type the renderer can sample works. A readback of
/// four 32x32 corner blocks happens when the buffer size changes; every other
/// frame is a lock and a compare.
pub fn popup_radius(renderer: &mut GlesRenderer, surface: &WlSurface) -> Option<f32> {
    let context = renderer.context_id();
    let (texture, scale) = with_renderer_surface_state(surface, |s| {
        Some((
            s.texture::<GlesTexture>(context)?.clone(),
            s.buffer_scale().max(1),
        ))
    })??;
    let size = (texture.width() as i32, texture.height() as i32);
    let cached = with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(|| Cached(Mutex::new(None)));
        let c = states.data_map.get::<Cached>()?.0.lock().ok()?;
        c.filter(|(s, _)| *s == size).map(|(_, r)| r)
    });
    let r = match cached {
        Some(r) => r,
        None => {
            let r = read_radius(renderer, &texture, size)?;
            with_states(surface, |states| {
                if let Some(Ok(mut c)) = states.data_map.get::<Cached>().map(|c| c.0.lock()) {
                    *c = Some((size, r));
                }
            });
            r
        }
    };
    Some(r / scale as f32)
}

fn read_radius(renderer: &mut GlesRenderer, texture: &GlesTexture, (w, h): (i32, i32)) -> Option<f32> {
    let n = SCAN.min(w / 2).min(h / 2);
    if n < 2 {
        return Some(0.0);
    }
    // One block per corner; the largest radius wins, as in `corner_radius`.
    let mut best = 0.0f32;
    for (x, y) in [(0, 0), (w - n, 0), (0, h - n), (w - n, h - n)] {
        let region = Rectangle::<i32, Buffer>::new((x, y).into(), (n, n).into());
        let mapping = renderer.copy_texture(texture, region, Fourcc::Abgr8888).ok()?;
        let bytes = renderer.map_texture(&mapping).ok()?;
        if bytes.len() < (n * n * 4) as usize {
            return None;
        }
        let r = corner_radius(n * 2, n * 2, |i, j| {
            // Only the block's own corner (0,0) is real; the rest reads opaque.
            if i < n && j < n {
                bytes[((j * n + i) * 4 + 3) as usize]
            } else {
                255
            }
        });
        best = best.max(r);
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Alpha of a `w x h` rect with corners rounded by `r`, hard-edged.
    fn rounded(w: i32, h: i32, r: f32) -> impl Fn(i32, i32) -> u8 {
        move |x, y| {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let cx = px.clamp(r, w as f32 - r);
            let cy = py.clamp(r, h as f32 - r);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            if d <= r {
                255
            } else {
                0
            }
        }
    }

    #[test]
    fn a_square_buffer_has_no_radius() {
        assert_eq!(corner_radius(100, 80, |_, _| 255), 0.0);
    }

    #[test]
    fn the_radius_is_recovered_from_the_corner_coverage() {
        for r in [4.0f32, 8.0, 12.0, 16.0] {
            let got = corner_radius(120, 90, rounded(120, 90, r));
            assert!((got - r).abs() < 1.2, "r {r} read as {got}");
        }
    }

    #[test]
    fn one_rounded_corner_is_enough() {
        let a = rounded(120, 90, 10.0);
        // Square off everything but the bottom-right corner.
        let alpha = |x: i32, y: i32| if x >= 60 && y >= 45 { a(x, y) } else { 255 };
        assert!((corner_radius(120, 90, alpha) - 10.0).abs() < 1.5);
    }

    #[test]
    fn a_tiny_buffer_is_square() {
        assert_eq!(corner_radius(2, 2, |_, _| 0), 0.0);
    }
}
