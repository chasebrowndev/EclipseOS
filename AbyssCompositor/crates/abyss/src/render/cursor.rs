// SPDX-License-Identifier: AGPL-3.0-only
//! Pointer rendering (COMP-02 §2). A client that sets its own cursor surface
//! gets that surface drawn at its hotspot; everything else — named shapes from
//! `wp_cursor_shape_v1`, and clients that never set one — gets the built-in
//! arrow below. There is deliberately no xcursor theme loader: a theme is a
//! filesystem dependency in the render path, and the trusted pointer must be
//! drawable whatever the user's theme directory happens to contain.

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
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    utils::{Physical, Point, Scale, Transform},
    wayland::compositor::with_states,
};

use crate::render::AbyssRenderElement;

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
/// Amber on black — the same accent the trusted UI uses.
const FILL: [u8; 4] = [38, 184, 255, 255];
const OUTLINE: [u8; 4] = [0, 0, 0, 255];

/// Argb8888 bytes for the arrow. Every pixel is fully opaque or fully clear,
/// so no premultiplication is needed.
fn arrow_pixels() -> Vec<u8> {
    let mut out = Vec::with_capacity((ARROW_W * ARROW_H * 4) as usize);
    for row in ARROW {
        let mut bytes = row.bytes();
        for _ in 0..ARROW_W {
            out.extend_from_slice(match bytes.next() {
                Some(b'x') => &FILL,
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
pub struct Fallback(Option<MemoryRenderBuffer>);

impl std::fmt::Debug for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fallback").finish_non_exhaustive()
    }
}

impl Fallback {
    fn get(&mut self) -> &MemoryRenderBuffer {
        self.0.get_or_insert_with(|| {
            MemoryRenderBuffer::from_slice(
                &arrow_pixels(),
                Fourcc::Argb8888,
                (ARROW_W, ARROW_H),
                1,
                Transform::Normal,
                None,
            )
        })
    }
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
        // Named shapes all get the arrow for now; the shape is advisory and a
        // wrong-looking pointer is better than no pointer at all.
        CursorImageStatus::Named(_) => {
            let pos: Point<i32, Physical> = location.to_physical_precise_round(scale);
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                pos.to_f64(),
                fallback.get(),
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
    fn arrow_pixels_are_argb8888_sized_to_the_arrow() {
        assert_eq!(arrow_pixels().len(), (ARROW_W * ARROW_H * 4) as usize);
    }

    #[test]
    fn arrow_has_an_opaque_tip_and_clear_corner() {
        let px = arrow_pixels();
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
}
