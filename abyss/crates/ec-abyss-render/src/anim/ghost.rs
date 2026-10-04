// SPDX-License-Identifier: AGPL-3.0-only
//! Ghosts: windows drawn after they have left the space (COMP-02 §9).
//!
//! The shell unmaps a window the moment it closes, minimizes or leaves the
//! active workspace, so `space`, `get_tree`, scene and hit-testing are done
//! with it at once. A ghost is the render path's leftover copy, played out
//! over one leaving [`Track`] and then dropped. Nothing outside
//! `collect_elements` ever sees one, and `capture.rs` builds its own pass from
//! `space`, so a ghost can never reach a screenshot or a screencast.
//!
//! Two kinds:
//! - [`Ghost::Live`]: the window still exists (outgoing workspace half,
//!   minimize) and is drawn from its live buffers.
//! - [`Ghost::Snapshot`]: the window is gone, or about to be (close). Its last
//!   textures were cloned beforehand by [`snapshot`]; a `GlesTexture` is an
//!   `Arc`, so the clone keeps the GPU texture alive after smithay drops the
//!   buffer, and nothing is copied.
//!
//! The shell pushes them from `ec-abyss/src/shell/anim.rs`: Live for the
//! outgoing workspace half, minimize and a window sent to another workspace;
//! Snapshot for close (taken in `compositor.rs commit()` before
//! `on_commit_buffer_handler` on `BufferAssignment::Removed`, in
//! `toplevel_destroyed` and the xwm unmap paths) and for the old frame of a
//! fullscreen or maximize toggle. A ghost is drawn with the border and shadow
//! its window had, in the focus state it left in.
//!
//! TODO(P4): `layer_destroyed` snapshots for `layer-close`.

use std::time::Instant;

use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
use smithay::backend::renderer::ContextId;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform as BufferTransform};
use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};

use super::track::Track;

/// One surface of a [`Ghost::Snapshot`]: its texture and where it sat.
#[derive(Debug, Clone)]
pub struct SnapshotSurface {
    /// Stable across frames, so the damage tracker sees one element moving
    /// rather than a new one every frame.
    pub id: Id,
    pub context: ContextId<GlesTexture>,
    pub texture: GlesTexture,
    /// Top-left relative to the window geometry's top-left, logical.
    pub offset: Point<i32, Logical>,
    /// Drawn size (the surface viewport's destination), logical.
    pub size: Size<i32, Logical>,
    /// Viewport source, logical.
    pub src: Rectangle<f64, Logical>,
    pub buffer_scale: i32,
    pub transform: BufferTransform,
}

/// A window drawn after it left the space.
#[derive(Debug)]
pub enum Ghost<W = Window> {
    Live {
        window: W,
        output: Output,
        /// Where its geometry's top-left was when it left, global logical.
        from_loc: Point<i32, Logical>,
        /// Whether it had the focus: picks its border colour and shadow depth.
        active: bool,
        track: Track,
    },
    Snapshot {
        /// Front to back, in `render_elements_from_surface_tree` order.
        surfaces: Vec<SnapshotSurface>,
        output: Output,
        /// The window geometry when the snapshot was taken, global logical.
        geometry: Rectangle<i32, Logical>,
        /// Whether it had the focus: picks its border colour and shadow depth.
        active: bool,
        track: Track,
    },
}

impl<W> Ghost<W> {
    pub fn track(&self) -> &Track {
        match self {
            Ghost::Live { track, .. } | Ghost::Snapshot { track, .. } => track,
        }
    }

    pub fn output(&self) -> &Output {
        match self {
            Ghost::Live { output, .. } | Ghost::Snapshot { output, .. } => output,
        }
    }

    pub fn active(&self) -> bool {
        match self {
            Ghost::Live { active, .. } | Ghost::Snapshot { active, .. } => *active,
        }
    }

    pub fn done(&self, now: Instant) -> bool {
        self.track().done(now)
    }

    /// The window a live ghost draws from; `None` for a snapshot.
    pub fn window(&self) -> Option<&W> {
        match self {
            Ghost::Live { window, .. } => Some(window),
            Ghost::Snapshot { .. } => None,
        }
    }
}

/// Clone the current textures of `surface`'s tree for a [`Ghost::Snapshot`].
///
/// `origin` is where the tree's root sits relative to the window geometry's
/// top-left — `-window.geometry().loc` for a toplevel. A surface with no
/// texture imported for `context` (never drawn, or a client that crashed
/// before its first frame) is skipped; an empty result means there is nothing
/// to animate, and the caller pushes no ghost.
pub fn snapshot(
    context: &ContextId<GlesTexture>,
    surface: &WlSurface,
    origin: Point<i32, Logical>,
) -> Vec<SnapshotSurface> {
    let mut out = Vec::new();
    // Same walk as smithay's `render_elements_from_surface_tree`, so the
    // order (and so the stacking) is the one the live window drew in.
    with_surface_tree_downward(
        surface,
        origin,
        |_, states, location| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return TraversalAction::SkipChildren;
            };
            match data.lock().unwrap().view() {
                Some(view) => TraversalAction::DoChildren(*location + view.offset),
                None => TraversalAction::SkipChildren,
            }
        },
        |_, states, location| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return;
            };
            let state = data.lock().unwrap();
            let (Some(view), Some(texture)) = (state.view(), state.texture::<GlesTexture>(context.clone()))
            else {
                return;
            };
            out.push(SnapshotSurface {
                id: Id::new(),
                context: context.clone(),
                texture: texture.clone(),
                offset: *location + view.offset,
                size: view.dst,
                src: view.src,
                buffer_scale: state.buffer_scale(),
                transform: state.buffer_transform(),
            });
        },
        |_, _, _| true,
    );
    out
}
