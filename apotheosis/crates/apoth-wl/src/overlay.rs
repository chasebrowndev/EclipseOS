// SPDX-License-Identifier: AGPL-3.0-only
//! The overlay subsurface (T-09 §5, COMP-02 §9 stacking): one `wl_subsurface` above the
//! window's main surface that carries the interface's sheets (palette, search, sheets). It
//! has its own `eclipse_glass_v1` object, so the compositor blurs everything composited
//! beneath it, the grid text and the window's own glass included, and a sheet reads as a
//! second layer of glass instead of a hole in the first.
//!
//! The subsurface stays in **synced** mode. A synced subsurface's commit is cached and
//! applied atomically with the parent's next commit, so a sheet (its buffer and its pane
//! list) and the frame under it always appear on the same compositor frame; desynced, the
//! sheet could land a frame before or after the grid it frosts. The cost is that an overlay
//! change must be followed by a main-surface commit; the render thread always presents
//! both in one pass. The subsurface has an empty input region: it never takes input, so
//! hit-testing is the main surface's, unchanged.
//!
//! Created without a buffer, so it is unmapped (and free) until the first overlay frame.
#![allow(unreachable_pub)] // `pub` items of crate-private structs

use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, delegate_noop};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use crate::event::WindowId;
use crate::glass::GlassPanes;
use crate::rawhandle::WlSurfaceHandle;
use crate::scale::Scale;
use crate::state::WlState;

delegate_noop!(WlState: ignore WlSubsurface);
delegate_noop!(WlState: ignore WlSurface);

/// The overlay of one window. Cloneable and `Send`: the render thread owns a clone.
#[derive(Clone, Debug)]
pub struct OverlaySurface {
    surface: WlSurface,
    sub: WlSubsurface,
    /// Present in fractional-scale mode; otherwise the integer buffer scale is set.
    viewport: Option<WpViewport>,
    glass: GlassPanes,
    conn: Connection,
}

impl OverlaySurface {
    /// The surface handle the swapchain is created on.
    pub fn handle(&self) -> WlSurfaceHandle {
        WlSurfaceHandle::new(self.conn.clone(), self.surface.clone())
    }

    /// The glass object of this subsurface.
    pub fn glass(&self) -> GlassPanes {
        self.glass.clone()
    }

    /// Match the main surface: `logical` size and `scale`. Pending state, applied by the
    /// subsurface's next commit (the present).
    pub fn configure(&self, logical: (u32, u32), scale: Scale) {
        if let Some(vp) = &self.viewport {
            if logical.0 > 0 && logical.1 > 0 {
                vp.set_destination(logical.0 as i32, logical.1 as i32);
            }
        } else {
            self.surface.set_buffer_scale(scale.integer_ceil());
        }
    }

    pub(crate) fn destroy(&self) {
        self.glass.destroy();
        if let Some(v) = &self.viewport {
            v.destroy();
        }
        self.sub.destroy();
        self.surface.destroy();
    }
}

impl WlState {
    /// The overlay subsurface of window `wi`, created on first use. `None` without the glass
    /// global: the overlay exists only to stack glass.
    pub(crate) fn overlay_surface(&mut self, id: WindowId) -> Option<OverlaySurface> {
        let wi = self.win_index(id)?;
        if let Some(o) = &self.windows[wi].extras.overlay {
            return Some(o.clone());
        }
        let m = self.extras.glass.as_ref()?;
        let surface = self.globals.compositor.create_surface(&self.qh, ());
        let sub = self.globals.subcompositor.get_subsurface(
            &surface,
            &self.windows[wi].surface,
            &self.qh,
            (),
        );
        sub.set_position(0, 0);
        sub.set_sync();
        let region = self.globals.compositor.create_region(&self.qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        let viewport = self.windows[wi]
            .viewport
            .as_ref()
            .and(self.globals.viewporter.as_ref())
            .map(|v| v.get_viewport(&surface, &self.qh, ()));
        let obj = m.get_glass(&surface, &self.qh, ());
        let glass = GlassPanes::new(obj, self.conn.clone(), self.extras.glass_available.clone());
        let o = OverlaySurface {
            surface,
            sub,
            viewport,
            glass,
            conn: self.conn.clone(),
        };
        self.windows[wi].extras.overlay = Some(o.clone());
        Some(o)
    }
}
