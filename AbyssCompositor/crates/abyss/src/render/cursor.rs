// SPDX-License-Identifier: AGPL-3.0-only
//! Pointer rendering (COMP-02 §2). A client that sets its own cursor surface
//! gets that surface drawn at its hotspot; everything else — named shapes from
//! `wp_cursor_shape_v1`, and clients that never set one — gets the built-in
//! arrow below. There is deliberately no xcursor theme loader: a theme is a
//! filesystem dependency in the render path, and the trusted pointer must be
//! drawable whatever the user's theme directory happens to contain.

use smithay::{
    backend::renderer::{
        element::{
            texture::{TextureBuffer, TextureRenderElement},
            Kind,
        },
        gles::{GlesRenderer, GlesTexture},
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

/// The arrow texture, uploaded once and kept for the life of the renderer.
#[derive(Default)]
pub struct Fallback(Option<TextureBuffer<GlesTexture>>);

impl std::fmt::Debug for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fallback").finish_non_exhaustive()
    }
}

impl Fallback {
    fn get(&mut self, renderer: &mut GlesRenderer) -> Option<&TextureBuffer<GlesTexture>> {
        if self.0.is_none() {
            match TextureBuffer::from_memory(
                renderer,
                &arrow_pixels(),
                smithay::backend::allocator::Fourcc::Argb8888,
                (ARROW_W, ARROW_H),
                false,
                1,
                Transform::Normal,
                None,
            ) {
                Ok(buffer) => self.0 = Some(buffer),
                Err(err) => {
                    tracing::error!(?err, "uploading the built-in cursor");
                    return None;
                }
            }
        }
        self.0.as_ref()
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
            let Some(buffer) = fallback.get(renderer) else {
                return Vec::new();
            };
            let pos: Point<i32, Physical> = location.to_physical_precise_round(scale);
            vec![AbyssRenderElement::Texture(
                TextureRenderElement::from_texture_buffer(
                    pos.to_f64(),
                    buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ),
            )]
        }
    }
}
