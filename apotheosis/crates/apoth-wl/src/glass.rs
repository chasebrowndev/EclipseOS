// SPDX-License-Identifier: AGPL-3.0-only
//! Compositor-drawn glass panes (`eclipse_glass_v1`, ADR 0081, COMP-02 §9): the optional
//! global of T-09 §3 that lets the interface leave the ground of its chips, sheets and
//! menus to the compositor.
//!
//! Bound when advertised (Abyss allowlists `apotheosis`, and only while its blur mode is
//! `glass`). [`GlassPanes`] is the per-window handle: `Send`, so the render thread sends the
//! pane list just before the present that commits the surface, which is how the compositor
//! latches panes and buffer together. An unchanged list sends nothing, so an idle window
//! costs no requests. Without the global [`crate::WlHandle::glass_panes`] is `None` and the
//! interface draws its own ground.
#![allow(unreachable_pub)] // `pub` items of crate-private structs

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ec_protocols::glass::client::eclipse_glass_manager_v1;
pub(crate) use ec_protocols::glass::client::eclipse_glass_manager_v1::EclipseGlassManagerV1;
use ec_protocols::glass::client::eclipse_glass_surface_v1::{self, EclipseGlassSurfaceV1};
use wayland_client::{Connection, Dispatch, QueueHandle};

use crate::state::WlState;

/// Bytes in one pane record: nine native-endian 32-bit fields.
pub const RECORD_BYTES: usize = 36;
/// Most panes one `set_panes` may carry.
pub const MAX_PANES: usize = 64;
/// State bit: the pointer is over the pane.
pub const HOVERED: u32 = 1;
/// State bit: the pane is being pressed.
pub const PRESSED: u32 = 2;

/// The look of a pane (the chip family).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// A cell at rest.
    Plain,
    /// The focused or selected cell: the same glass a step clearer.
    Focused,
    /// A put-away cell: a ghost rim only.
    Away,
    /// Nothing at rest; lifts like plain when hovered.
    Latent,
}

impl Tone {
    fn code(self) -> u32 {
        match self {
            Tone::Plain => 0,
            Tone::Focused => 1,
            Tone::Away => 2,
            Tone::Latent => 3,
        }
    }
}

/// One pane: a rounded box in surface-local logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pane {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Corner radius, logical px.
    pub radius: u32,
    pub tone: Tone,
    /// [`HOVERED`] | [`PRESSED`].
    pub state: u32,
    /// Ground opacity, 0..=255.
    pub alpha: u8,
}

/// Pack panes for `set_panes`. An empty box and the 65th pane are dropped (the compositor
/// would refuse them as protocol errors).
pub fn pack(panes: &[Pane]) -> Vec<u8> {
    let mut out = Vec::with_capacity(panes.len().min(MAX_PANES) * RECORD_BYTES);
    for p in panes.iter().filter(|p| p.w > 0 && p.h > 0).take(MAX_PANES) {
        let fields = [
            p.x.to_ne_bytes(),
            p.y.to_ne_bytes(),
            p.w.to_ne_bytes(),
            p.h.to_ne_bytes(),
            p.radius.to_ne_bytes(),
            0u32.to_ne_bytes(),
            p.tone.code().to_ne_bytes(),
            (p.state & (HOVERED | PRESSED)).to_ne_bytes(),
            u32::from(p.alpha).to_ne_bytes(),
        ];
        for f in fields {
            out.extend_from_slice(&f);
        }
    }
    out
}

/// The panes of one window. Cloneable and `Send`: hand a clone to the render thread.
#[derive(Clone)]
pub struct GlassPanes {
    obj: EclipseGlassSurfaceV1,
    conn: Connection,
    /// What the compositor holds, so an unchanged list sends nothing.
    last: Arc<Mutex<Vec<u8>>>,
    available: Arc<AtomicBool>,
}

impl std::fmt::Debug for GlassPanes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlassPanes")
            .field("available", &self.available())
            .finish_non_exhaustive()
    }
}

impl GlassPanes {
    pub(crate) fn new(
        obj: EclipseGlassSurfaceV1,
        conn: Connection,
        available: Arc<AtomicBool>,
    ) -> GlassPanes {
        GlassPanes {
            obj,
            conn,
            last: Arc::default(),
            available,
        }
    }

    /// Whether the compositor is drawing panes right now. When it is not, the interface
    /// draws its own ground.
    pub fn available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    /// Send `panes` if they differ from what was last sent. The compositor latches them on
    /// the next commit of the surface, so call this just before presenting. Returns whether
    /// anything was sent.
    pub fn set(&self, panes: &[Pane]) -> bool {
        let bytes = pack(panes);
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if *last == bytes {
            return false;
        }
        self.obj.set_panes(bytes.clone());
        *last = bytes;
        // The presenter flushes after its commit; this keeps the request ahead of it.
        let _ = self.conn.flush();
        true
    }

    /// Whether the list currently held by the compositor is empty.
    pub fn is_empty(&self) -> bool {
        self.last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    pub(crate) fn destroy(&self) {
        self.obj.destroy();
    }
}

impl Dispatch<EclipseGlassManagerV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &EclipseGlassManagerV1,
        event: eclipse_glass_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_glass_manager_v1::Event::Capability { available } = event {
            let on = available != 0;
            if state.extras.glass_available.swap(on, Ordering::Relaxed) != on {
                state.emit(crate::event::WlEvent::CapabilitiesChanged);
            }
        }
    }
}

impl Dispatch<EclipseGlassSurfaceV1, ()> for WlState {
    fn event(
        _: &mut Self,
        _: &EclipseGlassSurfaceV1,
        _: eclipse_glass_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl WlState {
    /// The glass handle of window `wi`, created on first use; `None` without the global.
    pub(crate) fn glass_panes(&mut self, wi: usize) -> Option<GlassPanes> {
        let m = self.extras.glass.as_ref()?;
        let w = &mut self.windows[wi];
        if w.extras.glass.is_none() {
            let obj = m.get_glass(&w.surface, &self.qh, ());
            w.extras.glass = Some(GlassPanes::new(
                obj,
                self.conn.clone(),
                self.extras.glass_available.clone(),
            ));
        }
        w.extras.glass.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(x: i32) -> Pane {
        Pane {
            x,
            y: 2,
            w: 30,
            h: 20,
            radius: 10,
            tone: Tone::Focused,
            state: HOVERED | 8,
            alpha: 200,
        }
    }

    fn field(b: &[u8], i: usize) -> [u8; 4] {
        b[i * 4..i * 4 + 4].try_into().unwrap()
    }

    #[test]
    fn a_record_is_nine_native_fields_in_wire_order() {
        let b = pack(&[pane(5)]);
        assert_eq!(b.len(), RECORD_BYTES);
        assert_eq!(i32::from_ne_bytes(field(&b, 0)), 5);
        assert_eq!(i32::from_ne_bytes(field(&b, 2)), 30);
        assert_eq!(u32::from_ne_bytes(field(&b, 4)), 10);
        assert_eq!(u32::from_ne_bytes(field(&b, 5)), 0);
        assert_eq!(u32::from_ne_bytes(field(&b, 6)), 1);
        // Unknown state bits never reach the wire.
        assert_eq!(u32::from_ne_bytes(field(&b, 7)), HOVERED);
        assert_eq!(u32::from_ne_bytes(field(&b, 8)), 200);
    }

    #[test]
    fn empty_boxes_and_the_65th_pane_are_dropped() {
        let mut v: Vec<Pane> = (0..70).map(pane).collect();
        v[3].w = 0;
        assert_eq!(pack(&v).len(), MAX_PANES * RECORD_BYTES);
        assert!(pack(&[]).is_empty());
    }
}
