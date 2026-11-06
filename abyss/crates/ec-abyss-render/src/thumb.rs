// SPDX-License-Identifier: AGPL-3.0-only
//! Window thumbnails for the compositor's own overlays (COMP-05 §7.1, C-24,
//! ADR 0090): offscreen textures drawn from a window's own surface tree at a
//! reduced size.
//!
//! **Pixels never leave the compositor.** A thumbnail is a `GlesTexture` held
//! here and drawn by [`crate::overview`]; nothing reads it back, exports it or
//! shares it. This module is state-free, so it also decides nothing about
//! *whether* a window may be drawn: the caller (`ec-abyss/src/overview/thumbs.rs`)
//! asks `render::capture::classify` first and calls [`ThumbStore::render`]
//! only for a window that came back clear or node-redacted. A window that is
//! not may have its texture dropped with [`ThumbStore::remove`].

use std::{collections::HashMap, time::Instant};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                texture::TextureBuffer,
                Kind,
            },
            gles::{GlesRenderer, GlesTexture},
            Bind, Color32F, Offscreen,
        },
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

/// Largest thumbnail edge, device px. A request beyond it is refused: the
/// overlays never ask for one, and it keeps a bad size from allocating.
pub const MAX_EDGE: i32 = 4096;

/// One window's thumbnail.
struct Thumb {
    buffer: TextureBuffer<GlesTexture>,
    texture: GlesTexture,
    size: Size<i32, Physical>,
    /// The caller's change key for the window when this was drawn.
    commit: u64,
    at: Instant,
}

/// What to draw: `surface`'s tree, the region `src` of it (surface-local
/// logical, normally the window's bounding box), into a `size` texture kept
/// under `key`. `commit` is the caller's change key, `now` stamps it.
pub struct Job<'a> {
    pub key: u64,
    pub surface: &'a WlSurface,
    pub src: Rectangle<i32, Logical>,
    pub size: Size<i32, Physical>,
    pub commit: u64,
    pub now: Instant,
}

/// The thumbnails, by the window's handle.
#[derive(Default)]
pub struct ThumbStore {
    thumbs: HashMap<u64, Thumb>,
}

impl std::fmt::Debug for ThumbStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbStore")
            .field("held", &self.thumbs.len())
            .finish()
    }
}

impl ThumbStore {
    /// How many textures are held. Zero whenever nothing is showing one.
    pub fn len(&self) -> usize {
        self.thumbs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.thumbs.is_empty()
    }

    pub fn contains(&self, key: u64) -> bool {
        self.thumbs.contains_key(&key)
    }

    /// What was last drawn for `key`: its change key, when, and its size.
    pub fn state_of(&self, key: u64) -> Option<(u64, Instant, Size<i32, Physical>)> {
        self.thumbs.get(&key).map(|t| (t.commit, t.at, t.size))
    }

    /// The buffer to draw `key`'s thumbnail from.
    pub fn buffer(&self, key: u64) -> Option<&TextureBuffer<GlesTexture>> {
        self.thumbs.get(&key).map(|t| &t.buffer)
    }

    /// Drop `key`'s texture.
    pub fn remove(&mut self, key: u64) {
        self.thumbs.remove(&key);
    }

    /// Drop every texture whose key `keep` refuses.
    pub fn retain(&mut self, keep: impl Fn(u64) -> bool) {
        self.thumbs.retain(|k, _| keep(*k));
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.thumbs.clear();
    }

    /// Draw one [`Job`]. False when the texture could not be made or drawn;
    /// nothing is kept then.
    pub fn render(&mut self, renderer: &mut GlesRenderer, job: &Job<'_>) -> bool {
        let Job {
            key,
            surface,
            src,
            size,
            commit,
            now,
        } = *job;
        if size.w <= 0
            || size.h <= 0
            || size.w > MAX_EDGE
            || size.h > MAX_EDGE
            || src.size.w <= 0
            || src.size.h <= 0
        {
            self.thumbs.remove(&key);
            return false;
        }
        let scale = Scale::from(f64::from(size.w) / f64::from(src.size.w));
        // The same size as last time keeps the texture: it is redrawn in place.
        let existing = self.thumbs.remove(&key).filter(|t| t.size == size);
        let mut texture = match existing.map(|t| t.texture) {
            Some(t) => t,
            None => {
                let want: Size<i32, BufferCoords> = size.to_logical(1).to_buffer(1, Transform::Normal);
                match Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, want) {
                    Ok(t) => t,
                    Err(err) => {
                        tracing::warn!(?err, "allocating a thumbnail texture");
                        return false;
                    }
                }
            }
        };
        let at: Point<i32, Physical> = Point::<i32, Logical>::from((-src.loc.x, -src.loc.y))
            .to_f64()
            .to_physical(scale)
            .to_i32_round();
        let elements = render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<GlesRenderer>>(
            renderer,
            surface,
            at,
            scale,
            1.0,
            Kind::Unspecified,
        );
        let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        let drawn = (|| {
            let mut fb = Bind::bind(renderer, &mut texture).map_err(|_| ())?;
            tracker
                .render_output(renderer, &mut fb, 0, &elements, Color32F::new(0.0, 0.0, 0.0, 0.0))
                .map(|_| ())
                .map_err(|_| ())
        })();
        if drawn.is_err() {
            tracing::warn!("drawing a thumbnail failed");
            return false;
        }
        // A new buffer, so its element gets a new id and the damage tracker
        // sees the redrawn pixels as new.
        let buffer = TextureBuffer::from_texture(renderer, texture.clone(), 1, Transform::Normal, None);
        self.thumbs.insert(
            key,
            Thumb {
                buffer,
                texture,
                size,
                commit,
                at: now,
            },
        );
        true
    }
}
