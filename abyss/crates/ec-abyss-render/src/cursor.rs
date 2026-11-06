// SPDX-License-Identifier: AGPL-3.0-only
//! Pointer rendering (COMP-02 §2). A client that sets its own cursor surface
//! gets that surface drawn at its hotspot; everything else — named shapes from
//! `wp_cursor_shape_v1`, and clients that never set one — gets the built-in
//! arrow below. There is deliberately no xcursor theme loader: a theme is a
//! filesystem dependency in the render path, and the trusted pointer must be
//! drawable whatever the user's theme directory happens to contain. The arrow's
//! fill is the shared accent (see [`set_accent`]) with a legibility floor.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    utils::{Physical, Point, Scale, Transform},
    wayland::compositor::with_states,
};

use std::sync::atomic::{AtomicU32, Ordering};

use ec_theme::Rgba;

use crate::AbyssRenderElement;

/// The built-in arrow, one character per pixel: `o` outline, `x` fill.
const ARROW: [&str; 19] = [
    "o...........",
    "oo..........",
    "oxo.........",
    "oxxo........",
    "oxxxo.......",
    "oxxxxo......",
    "oxxxxxo.....",
    "oxxxxxxo....",
    "oxxxxxxxo...",
    "oxxxxxxxxo..",
    "oxxxxxxxxxo.",
    "oxxxxxxoooo.",
    "oxxxoxxo....",
    "oxxo.oxxo...",
    "oxo..oxxo...",
    "oo....oxxo..",
    "o.....oxxo..",
    ".......oxo..",
    ".......oo...",
];
const ARROW_W: i32 = 12;
const ARROW_H: i32 = ARROW.len() as i32;
const OUTLINE: [u8; 4] = [0, 0, 0, 255];

/// The shared accent (`0xRRGGBB`) the arrow is filled with; defaults to the
/// built-in gold. Process-global because this crate is state-free: the
/// compositor sets it when the effective accent changes and [`Fallback`]
/// rebuilds its buffer when it sees a different value.
static ACCENT: AtomicU32 = AtomicU32::new(0xf2c33c);

/// Set the pointer's accent (`0xRRGGBB`). Cheap; call on every theme refresh.
pub fn set_accent(rgb: u32) {
    ACCENT.store(rgb & 0x00ff_ffff, Ordering::Relaxed);
}

/// Minimum WCAG relative luminance of the fill, so the arrow stays visible
/// on any accent (the black outline already covers light fills).
const MIN_FILL_LUMINANCE: f32 = 0.2;

/// The fill for `accent`: the accent itself, mixed toward white in sRGB just
/// far enough to reach [`MIN_FILL_LUMINANCE`] when it is darker than that.
/// Returns Argb8888 little-endian bytes (B, G, R, A).
fn fill_for(accent: u32) -> [u8; 4] {
    let ch = |shift: u32| ((accent >> shift) & 0xff) as f32 / 255.0;
    let base = Rgba::new(ch(16), ch(8), ch(0), 1.0);
    let mut out = base;
    for step in 0..=20 {
        let t = step as f32 / 20.0;
        out = Rgba::new(
            base.r() + (1.0 - base.r()) * t,
            base.g() + (1.0 - base.g()) * t,
            base.b() + (1.0 - base.b()) * t,
            1.0,
        );
        if out.luminance() >= MIN_FILL_LUMINANCE {
            break;
        }
    }
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [b(out.b()), b(out.g()), b(out.r()), 255]
}

/// Argb8888 bytes for the arrow. Every pixel is fully opaque or fully clear,
/// so no premultiplication is needed.
fn arrow_pixels(accent: u32) -> Vec<u8> {
    let fill = fill_for(accent);
    let mut out = Vec::with_capacity((ARROW_W * ARROW_H * 4) as usize);
    for row in ARROW {
        let mut bytes = row.bytes();
        for _ in 0..ARROW_W {
            out.extend_from_slice(match bytes.next() {
                Some(b'x') => &fill,
                Some(b'o') => &OUTLINE,
                _ => &[0, 0, 0, 0],
            });
        }
    }
    out
}

/// The largest cursor plane any KMS driver exposes is at least this big; the
/// arrow must fit or `DrmCompositor` composites it instead of scanning it out.
const MIN_CURSOR_PLANE: i32 = 64;
const _: () = assert!(ARROW_W <= MIN_CURSOR_PLANE && ARROW_H <= MIN_CURSOR_PLANE);

/// The arrow, built once and kept for the life of the renderer. It is a
/// memory buffer rather than a texture on purpose: smithay only scans an
/// element out on a plane when it has underlying storage (COMP-02 §5), and a
/// texture element has none, so the arrow would always be composited.
#[derive(Default)]
pub struct Fallback(
    Option<(u32, MemoryRenderBuffer)>,
    [Option<(u32, MemoryRenderBuffer)>; 4],
);

impl std::fmt::Debug for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fallback").finish_non_exhaustive()
    }
}

impl Fallback {
    /// The arrow buffer for the current accent. A new accent builds a fresh
    /// buffer (new id, so the renderer re-uploads and the cursor plane updates);
    /// an unchanged one reuses it with no allocation.
    /// The resize glyph for `axis`, built like [`Fallback::get`].
    fn get_resize(&mut self, axis: Axis) -> &MemoryRenderBuffer {
        let accent = ACCENT.load(Ordering::Relaxed);
        let slot = &mut self.1[axis as usize];
        if slot.as_ref().is_none_or(|(a, _)| *a != accent) {
            let buf = MemoryRenderBuffer::from_slice(
                &resize_pixels(axis, accent),
                Fourcc::Argb8888,
                (GLYPH, GLYPH),
                1,
                Transform::Normal,
                None,
            );
            *slot = Some((accent, buf));
        }
        &slot.as_ref().expect("just built").1
    }

    fn get(&mut self) -> &MemoryRenderBuffer {
        let accent = ACCENT.load(Ordering::Relaxed);
        if self.0.as_ref().is_none_or(|(a, _)| *a != accent) {
            let buf = MemoryRenderBuffer::from_slice(
                &arrow_pixels(accent),
                Fourcc::Argb8888,
                (ARROW_W, ARROW_H),
                1,
                Transform::Normal,
                None,
            );
            self.0 = Some((accent, buf));
        }
        &self.0.as_ref().expect("just built").1
    }
}

/// Resize glyph edge, device px.
const GLYPH: i32 = 24;
const _: () = assert!(GLYPH <= MIN_CURSOR_PLANE);

/// Which double arrow a resize shape is drawn as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Ns = 0,
    Ew = 1,
    Nesw = 2,
    Nwse = 3,
}

/// The glyph axis of a resize cursor, `None` for any other shape.
pub fn resize_axis(icon: CursorIcon) -> Option<Axis> {
    use CursorIcon as I;
    match icon {
        I::NResize | I::SResize | I::NsResize | I::RowResize => Some(Axis::Ns),
        I::EResize | I::WResize | I::EwResize | I::ColResize => Some(Axis::Ew),
        I::NeResize | I::SwResize | I::NeswResize => Some(Axis::Nesw),
        I::NwResize | I::SeResize | I::NwseResize => Some(Axis::Nwse),
        _ => None,
    }
}

/// Is `(x, y)` (glyph-centred, device px) inside the double arrow along `axis`?
fn in_arrow(axis: Axis, x: f32, y: f32) -> bool {
    // Rotate into the arrow's own frame: `u` along it, `v` across.
    let (u, v) = match axis {
        Axis::Ew => (x, y),
        Axis::Ns => (y, x),
        // y grows downward: NE-SW runs up and to the right.
        Axis::Nesw => (
            (x - y) * std::f32::consts::FRAC_1_SQRT_2,
            (x + y) * std::f32::consts::FRAC_1_SQRT_2,
        ),
        Axis::Nwse => (
            (x + y) * std::f32::consts::FRAC_1_SQRT_2,
            (y - x) * std::f32::consts::FRAC_1_SQRT_2,
        ),
    };
    let (au, av) = (u.abs(), v.abs());
    let shaft = au <= 7.0 && av <= 1.2;
    let head = (5.0..=10.0).contains(&au) && av <= (10.0 - au) * 0.9;
    shaft || head
}

/// Argb8888 bytes for a resize glyph: the accent fill with a black outline.
fn resize_pixels(axis: Axis, accent: u32) -> Vec<u8> {
    let fill = fill_for(accent);
    let n = GLYPH as usize;
    let c = GLYPH as f32 / 2.0;
    let inside = |px: i32, py: i32| {
        px >= 0
            && py >= 0
            && px < GLYPH
            && py < GLYPH
            && in_arrow(axis, px as f32 + 0.5 - c, py as f32 + 0.5 - c)
    };
    let mut out = Vec::with_capacity(n * n * 4);
    for py in 0..GLYPH {
        for px in 0..GLYPH {
            let px_bytes: &[u8] = if inside(px, py) {
                &fill
            } else if (-1..=1).any(|dy| (-1..=1).any(|dx| inside(px + dx, py + dy))) {
                &OUTLINE
            } else {
                &[0, 0, 0, 0]
            };
            out.extend_from_slice(px_bytes);
        }
    }
    out
}

/// Elements for the pointer at `location` (output-local, logical). Empty when
/// the cursor is hidden.
pub fn elements(
    renderer: &mut GlesRenderer,
    status: &CursorImageStatus,
    fallback: &mut Fallback,
    location: Point<f64, smithay::utils::Logical>,
    scale: Scale<f64>,
) -> Vec<AbyssRenderElement> {
    match status {
        CursorImageStatus::Hidden => Vec::new(),
        CursorImageStatus::Surface(surface) => {
            // The hotspot is the point inside the surface that tracks the
            // pointer, so the surface is drawn back by that much.
            let hotspot = with_states(surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|d| d.lock().unwrap().hotspot)
                    .unwrap_or_default()
            });
            let pos: Point<i32, Physical> = (location - hotspot.to_f64()).to_physical_precise_round(scale);
            smithay::backend::renderer::element::surface::render_elements_from_surface_tree(
                renderer,
                surface,
                pos,
                scale,
                1.0,
                Kind::Cursor,
            )
            .into_iter()
            .map(AbyssRenderElement::Surface)
            .collect()
        }
        // The resize shapes (a window edge or corner under `mode "de"`) get a
        // double arrow centred on the pointer. Every other named shape gets the
        // arrow for now; the shape is advisory and a wrong-looking pointer is
        // better than no pointer at all.
        CursorImageStatus::Named(icon) => {
            let axis = resize_axis(*icon);
            let at = match axis {
                Some(_) => location - Point::from((GLYPH / 2, GLYPH / 2)).to_f64(),
                None => location,
            };
            let pos: Point<i32, Physical> = at.to_physical_precise_round(scale);
            let buffer = match axis {
                Some(axis) => fallback.get_resize(axis),
                None => fallback.get(),
            };
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                pos.to_f64(),
                buffer,
                None,
                None,
                None,
                Kind::Cursor,
            ) {
                Ok(element) => vec![AbyssRenderElement::Memory(element)],
                Err(err) => {
                    tracing::error!(?err, "uploading the built-in cursor");
                    Vec::new()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_shapes_map_to_their_axis_and_others_do_not() {
        assert_eq!(resize_axis(CursorIcon::NResize), Some(Axis::Ns));
        assert_eq!(resize_axis(CursorIcon::SResize), Some(Axis::Ns));
        assert_eq!(resize_axis(CursorIcon::WResize), Some(Axis::Ew));
        assert_eq!(resize_axis(CursorIcon::NwResize), Some(Axis::Nwse));
        assert_eq!(resize_axis(CursorIcon::SeResize), Some(Axis::Nwse));
        assert_eq!(resize_axis(CursorIcon::NeResize), Some(Axis::Nesw));
        assert_eq!(resize_axis(CursorIcon::SwResize), Some(Axis::Nesw));
        assert_eq!(resize_axis(CursorIcon::Default), None);
        assert_eq!(resize_axis(CursorIcon::Text), None);
    }

    #[test]
    fn a_resize_glyph_is_drawn_along_its_axis_and_centred() {
        let at = |px: &[u8], x: usize, y: usize| px[(y * GLYPH as usize + x) * 4 + 3];
        let c = GLYPH as usize / 2;
        let ew = resize_pixels(Axis::Ew, 0xf2c33c);
        assert_eq!(ew.len(), (GLYPH * GLYPH * 4) as usize);
        assert_eq!(at(&ew, c, c), 255, "the centre is filled");
        assert_eq!(at(&ew, c + 8, c), 255, "east head");
        assert_eq!(at(&ew, c - 9, c), 255, "west head");
        assert_eq!(at(&ew, c, c - 8), 0, "nothing north of an EW arrow");
        let ns = resize_pixels(Axis::Ns, 0xf2c33c);
        assert_eq!(at(&ns, c, c + 8), 255);
        assert_eq!(at(&ns, c + 8, c), 0);
        let nesw = resize_pixels(Axis::Nesw, 0xf2c33c);
        assert_eq!(at(&nesw, c + 6, c - 6), 255, "up and to the right");
        assert_eq!(at(&nesw, c - 6, c - 6), 0, "not up and to the left");
        let nwse = resize_pixels(Axis::Nwse, 0xf2c33c);
        assert_eq!(at(&nwse, c - 6, c - 6), 255);
        assert_eq!(at(&nwse, c + 6, c - 6), 0);
    }

    #[test]
    fn arrow_pixels_are_argb8888_sized_to_the_arrow() {
        assert_eq!(arrow_pixels(0xf2c33c).len(), (ARROW_W * ARROW_H * 4) as usize);
    }

    #[test]
    fn arrow_has_an_opaque_tip_and_clear_corner() {
        let px = arrow_pixels(0xf2c33c);
        let at = |x: usize, y: usize| &px[(y * ARROW_W as usize + x) * 4..][..4];
        assert_eq!(at(0, 0), OUTLINE, "the hotspot pixel is the outline");
        assert_eq!(
            at(ARROW_W as usize - 1, 0),
            [0, 0, 0, 0],
            "the far corner is clear"
        );
    }

    #[test]
    fn fallback_builds_once_and_reuses_the_buffer() {
        let mut fallback = Fallback::default();
        let first: *const MemoryRenderBuffer = fallback.get();
        let second: *const MemoryRenderBuffer = fallback.get();
        assert!(
            std::ptr::eq(first, second),
            "one buffer, so the texture upload is cached"
        );
    }

    #[test]
    fn fill_is_bgra_ordered_and_default_gold_is_unchanged() {
        assert_eq!(fill_for(0xf2c33c), [0x3c, 0xc3, 0xf2, 255]);
        let px = arrow_pixels(0x102030 | 0x808080);
        let at = |x: usize, y: usize| &px[(y * ARROW_W as usize + x) * 4..][..4];
        assert_eq!(at(1, 2), [0xb0, 0xa0, 0x90, 255]);
    }

    #[test]
    fn dark_accents_are_lifted_and_light_ones_untouched() {
        let lum = |f: [u8; 4]| Rgba::rgb(u32::from_be_bytes([0, f[2], f[1], f[0]])).luminance();
        for a in [0x000000, 0x101010, 0x0a0a30] {
            let f = fill_for(a);
            assert!(lum(f) >= MIN_FILL_LUMINANCE, "{a:06x} -> {f:?}");
            assert_eq!(f[3], 255);
        }
        assert_eq!(fill_for(0xffffff), [255, 255, 255, 255]);
        for a in [0xf2c33c, 0x4da3ff, 0xff5c8a, 0x3ddc84] {
            let f = fill_for(a);
            assert_eq!(
                f,
                [(a & 0xff) as u8, ((a >> 8) & 0xff) as u8, (a >> 16) as u8, 255]
            );
        }
    }

    #[test]
    fn fallback_rebuilds_only_when_the_accent_changes() {
        // ACCENT is process-global: keep every mutation in this one test.
        let built = |f: &Fallback| f.0.as_ref().map(|(a, _)| *a);
        set_accent(0x4da3ff);
        let mut fallback = Fallback::default();
        let first: *const MemoryRenderBuffer = fallback.get();
        assert_eq!(built(&fallback), Some(0x4da3ff));
        let again: *const MemoryRenderBuffer = fallback.get();
        assert!(std::ptr::eq(first, again), "unchanged accent reuses the buffer");
        set_accent(0xff5c8a);
        fallback.get();
        assert_eq!(built(&fallback), Some(0xff5c8a), "a new accent rebuilds");
        set_accent(0xf2c33c);
    }
}
