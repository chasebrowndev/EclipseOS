// SPDX-License-Identifier: AGPL-3.0-only
//! The shell's side of window animation (COMP-02 §9): where ghosts and
//! arrive tracks are started.
//!
//! Everything here is render-only bookkeeping handed to
//! `state.borders.anim`. The shell has already mapped, unmapped or resized
//! the window by the time a track runs, so `get_tree`, scene and hit-testing
//! see the target and nothing else. A ghost is never in `space`, never
//! listed, never hit-tested, and `capture.rs` never reads it.
//!
//! Each call resolves its event from `animations` and does nothing when it is
//! off, when the session is locked, or when the window has nothing left to
//! draw — a client that crashed gets no animation and no panic.

use std::time::Instant;

use smithay::desktop::{layer_map_for_output, LayerSurface, Window};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle};

use ec_abyss_render::anim::{self, Channels, Ghost, Leg, SnapshotSurface, Track};

use crate::config::animations::Event;
use crate::state::AbyssState;

/// The layer-shell namespace of the taskbar a minimizing window shrinks into.
const BAR_NAMESPACE: &str = "hyperion";

/// A window's last frame: what a [`Ghost::Snapshot`] is made from.
pub struct Frame {
    pub surfaces: Vec<SnapshotSurface>,
    pub output: Output,
    pub geometry: Rectangle<i32, Logical>,
    pub active: bool,
}

fn resolve(state: &AbyssState, ev: Event) -> Option<(Leg, &'static str)> {
    if state.lock.locked {
        return None;
    }
    anim::resolve(
        &state.config.animations,
        ev,
        Instant::now(),
        state.borders.anim.shed(),
    )
}

fn centre(r: &Rectangle<i32, Logical>) -> (f64, f64) {
    (
        r.loc.x as f64 + r.size.w as f64 / 2.0,
        r.loc.y as f64 + r.size.h as f64 / 2.0,
    )
}

/// The displacement that draws a window laid out at `at` as if it sat at
/// `from`: the track a window *arrives* with when it comes from `from`, and
/// (with the arguments swapped) the one a ghost *leaves* with to land on it.
fn displace(from: &Rectangle<i32, Logical>, at: &Rectangle<i32, Logical>) -> Channels {
    let (f, a) = (centre(from), centre(at));
    let ratio = |x: i32, y: i32| if y > 0 { x as f64 / y as f64 - 1.0 } else { 0.0 };
    Channels {
        dx: f.0 - a.0,
        dy: f.1 - a.1,
        sx: ratio(from.size.w, at.size.w),
        sy: ratio(from.size.h, at.size.h),
        alpha: 0.0,
    }
}

const FADE: Channels = Channels {
    alpha: -1.0,
    ..Channels::ZERO
};

fn output_for(state: &AbyssState, geometry: &Rectangle<i32, Logical>) -> Option<Output> {
    let (x, y) = centre(geometry);
    super::output_at(state, Point::from((x, y)))
}

/// Clone the textures `window` is showing. `None` when there is nothing to
/// keep: not mapped, no renderer context yet (nothing has been drawn), the
/// surface already gone, or no texture imported for it.
pub fn capture(state: &AbyssState, window: &Window) -> Option<Frame> {
    let context = state.borders.anim.context.as_ref()?;
    let geometry = state.space.element_geometry(window)?;
    let surface = super::window_surface(window).filter(|s| s.alive())?;
    let surfaces = anim::snapshot(context, &surface, Point::from((0, 0)) - window.geometry().loc);
    if surfaces.is_empty() {
        return None;
    }
    Some(Frame {
        surfaces,
        output: output_for(state, &geometry)?,
        geometry,
        active: state.focus.as_ref() == Some(window),
    })
}

fn push_snapshot(state: &mut AbyssState, frame: Frame, track: Track) {
    state.borders.anim.push_ghost(Ghost::Snapshot {
        surfaces: frame.surfaces,
        output: frame.output,
        geometry: frame.geometry,
        active: frame.active,
        track,
    });
}

/// A window still alive but leaving the space: drawn from its live buffers.
/// Called *before* it is unmapped, while its geometry is still known.
fn push_live(state: &mut AbyssState, window: &Window, output: &Output, track: Track) {
    if !window.alive() {
        return;
    }
    let Some(geo) = state.space.element_geometry(window) else {
        return;
    };
    let active = state.focus.as_ref() == Some(window);
    state.borders.anim.push_ghost(Ghost::Live {
        window: window.clone(),
        output: output.clone(),
        from_loc: geo.loc,
        active,
        track,
    });
}

/// `window-close`: keep the window's last frame and play it out. Call while
/// the window is still in the space and its buffer still held — before
/// `on_commit_buffer_handler` drops it on a null-buffer commit, or before the
/// unmap on destroy.
pub fn close(state: &mut AbyssState, window: &Window) {
    if resolve(state, Event::WindowClose).is_none() {
        return;
    }
    if let Some(frame) = capture(state, window) {
        close_frame(state, frame);
    }
}

/// [`close`] once the frame is in hand. The ghost owns cloned textures, so it
/// outlives the buffer, the surface and the client.
pub fn close_frame(state: &mut AbyssState, frame: Frame) {
    let Some((leg, style)) = resolve(state, Event::WindowClose) else {
        return;
    };
    push_snapshot(state, frame, Track::leave(leg, anim::close_to(style)));
}

/// `layer-close`: keep a layer surface's last frame and play it out, leaving
/// toward the edge it opened from. Call while the surface is still in its
/// output's layer map and its buffer still held, so on a null-buffer commit
/// before `on_commit_buffer_handler`, and in `layer_destroyed` before the
/// unmap. A surface that covers its whole output (a scrim) is not animated.
/// Called twice for one surface (the null commit, then the destroy) it keeps
/// one frame: the second finds the buffer gone and has nothing to snapshot.
pub fn close_layer(state: &mut AbyssState, output: &Output, layer: &LayerSurface) {
    let Some((leg, style)) = resolve(state, Event::LayerClose) else {
        return;
    };
    let Some(context) = state.borders.anim.context.as_ref() else {
        return;
    };
    let Some(output_size) = state.space.output_geometry(output).map(|g| g.size) else {
        return;
    };
    let (geometry, surface) = {
        let map = layer_map_for_output(output);
        let Some(geometry) = super::layer_geometry(&map, layer) else {
            return;
        };
        (geometry, layer.wl_surface().clone())
    };
    let anchor = layer.cached_state().anchor;
    let Some(end) = anim::layer_motion(style, anchor, geometry.size, output_size) else {
        return;
    };
    if !surface.alive() {
        return;
    }
    let surfaces = anim::snapshot(context, &surface, Point::from((0, 0)));
    if surfaces.is_empty() {
        return;
    }
    state.borders.anim.push_ghost(Ghost::Layer {
        layer: layer.layer(),
        surfaces,
        output: output.clone(),
        geometry,
        track: Track::leave(leg, end),
    });
}

/// [`close_layer`] for the layer surface owning `surface`, if it is one.
pub fn close_layer_of(state: &mut AbyssState, surface: &WlSurface) {
    let found = state.outputs.iter().map(|e| e.output.clone()).find_map(|output| {
        let layer = layer_map_for_output(&output)
            .layer_for_surface(surface, smithay::desktop::WindowSurfaceType::TOPLEVEL)
            .cloned()?;
        Some((output, layer))
    });
    if let Some((output, layer)) = found {
        close_layer(state, &output, &layer);
    }
}

/// `workspace-switch`, outgoing half: `windows` leave opposite to where the
/// arrivals come from (`dir` +1 = new workspace is to the right/below).
/// Called before they are unmapped.
pub fn leave_workspace(state: &mut AbyssState, windows: &[Window], output: &Output, dir: i32) {
    let Some((leg, style)) = resolve(state, Event::WorkspaceSwitch) else {
        return;
    };
    let size = state
        .space
        .output_geometry(output)
        .map_or_else(Default::default, |g| g.size);
    let end = match style {
        "fade" => FADE,
        "slide-vertical" => Channels {
            dy: -(dir * size.h) as f64,
            ..Channels::ZERO
        },
        _ => Channels {
            dx: -(dir * size.w) as f64,
            ..Channels::ZERO
        },
    };
    for window in windows {
        push_live(state, window, output, Track::leave(leg, end));
    }
}

/// `window-to-workspace`, a window that is *not* followed: it leaves toward
/// the side the target workspace is on (`toward` +1 right, −1 left). Called
/// before it is unmapped.
pub fn send_away(state: &mut AbyssState, window: &Window, output: &Output, toward: i32) {
    let Some((leg, style)) = resolve(state, Event::WindowToWorkspace) else {
        return;
    };
    let width = state.space.output_geometry(output).map_or(0, |g| g.size.w);
    let end = match style {
        "fade" => FADE,
        _ => Channels {
            dx: (toward * width) as f64 * 0.3,
            sx: -0.1,
            sy: -0.1,
            alpha: -1.0,
            ..Channels::ZERO
        },
    };
    push_live(state, window, output, Track::leave(leg, end));
}

/// `window-to-workspace`, a window that is followed: the workspace slides
/// under it (`carry`), so it glides from where it was to where it lands
/// instead of riding in with the others; or it fades in. `from` is its
/// geometry before the move. Call after the switch, which would otherwise
/// slide it in with the rest.
pub fn carried(state: &mut AbyssState, window: &Window, from: Rectangle<i32, Logical>) {
    let Some((leg, style)) = resolve(state, Event::WindowToWorkspace) else {
        return;
    };
    let Some(to) = state.space.element_geometry(window) else {
        return;
    };
    let start = match style {
        "fade" => FADE,
        _ => displace(&from, &to),
    };
    state
        .borders
        .anim
        .arrive(&state.space, window, Track::arrive(leg, start));
}

/// `window-to-workspace` across displays: it leaves its old output toward the
/// new one ([`leave_output`], before the unmap), and fades in on the new one
/// ([`arrive_on_output`], after the arrange).
pub fn leave_output(state: &mut AbyssState, window: &Window, old: &Output, new: &Output) {
    let toward = match (state.space.output_geometry(old), state.space.output_geometry(new)) {
        (Some(a), Some(b)) if b.loc.x < a.loc.x => -1,
        _ => 1,
    };
    send_away(state, window, old, toward);
}

pub fn arrive_on_output(state: &mut AbyssState, window: &Window) {
    if let Some((leg, _)) = resolve(state, Event::WindowToWorkspace) {
        state
            .borders
            .anim
            .arrive(&state.space, window, Track::arrive(leg, FADE));
    }
}

/// Where a minimizing window shrinks to and unminimizing grows from: the
/// taskbar's centre when it is up, else the output's bottom centre.
fn dock(state: &AbyssState, output: &Output) -> Option<(f64, f64)> {
    let out = state.space.output_geometry(output)?;
    let map = layer_map_for_output(output);
    let bar = map
        .layers()
        .find(|l| l.namespace() == BAR_NAMESPACE)
        .and_then(|l| super::layer_geometry(&map, l));
    Some(match bar {
        Some(geo) => {
            let c = centre(&geo);
            (out.loc.x as f64 + c.0, out.loc.y as f64 + c.1)
        }
        None => (
            out.loc.x as f64 + out.size.w as f64 / 2.0,
            (out.loc.y + out.size.h) as f64,
        ),
    })
}

/// The displacement that puts a window at `geo` onto `dock`, shrunk.
fn shrunk(geo: &Rectangle<i32, Logical>, dock: (f64, f64)) -> Channels {
    let c = centre(geo);
    Channels {
        dx: dock.0 - c.0,
        dy: dock.1 - c.1,
        sx: -0.9,
        sy: -0.9,
        alpha: -1.0,
    }
}

/// `minimize`: shrink toward the bar, or fade. Called before the unmap.
pub fn minimize(state: &mut AbyssState, window: &Window) {
    let Some((leg, style)) = resolve(state, Event::Minimize) else {
        return;
    };
    let Some(geo) = state.space.element_geometry(window) else {
        return;
    };
    let Some(output) = output_for(state, &geo) else {
        return;
    };
    let end = match (style, dock(state, &output)) {
        ("shrink", Some(dock)) => shrunk(&geo, dock),
        _ => FADE,
    };
    push_live(state, window, &output, Track::leave(leg, end));
}

/// `unminimize`: grow back out of the bar, or fade in. Called once the window
/// is mapped again; one restored onto a hidden workspace is not in the space
/// and is skipped.
pub fn unminimize(state: &mut AbyssState, window: &Window) {
    let Some((leg, style)) = resolve(state, Event::Unminimize) else {
        return;
    };
    let Some(geo) = state.space.element_geometry(window) else {
        return;
    };
    let from = match (style, output_for(state, &geo).and_then(|o| dock(state, &o))) {
        ("shrink", Some(dock)) => shrunk(&geo, dock),
        _ => FADE,
    };
    state
        .borders
        .anim
        .arrive(&state.space, window, Track::arrive(leg, from));
}

/// Run a fullscreen or maximize change on `window` (`None`: it has no
/// `Window` yet, so nothing to animate) and play `fullscreen` over it.
///
/// `morph`: the old frame, scaled and moved onto the new rectangle, crossfades
/// with the live window coming from the old one. `fade`: the old frame fades
/// where it was while the live window fades in. With no snapshot to keep (no
/// texture yet) the live window still morphs.
pub fn toggle(state: &mut AbyssState, window: Option<Window>, change: impl FnOnce(&mut AbyssState)) {
    let before = window.as_ref().and_then(|w| {
        resolve(state, Event::Fullscreen)?;
        let old = state.space.element_geometry(w)?;
        Some((old, capture(state, w)))
    });
    change(state);
    let (Some(window), Some((old, frame))) = (window, before) else {
        return;
    };
    let Some((leg, style)) = resolve(state, Event::Fullscreen) else {
        return;
    };
    let Some(new) = state.space.element_geometry(&window) else {
        return;
    };
    if new == old {
        return;
    }
    let morph = style == "morph";
    let mut from = if morph {
        displace(&old, &new)
    } else {
        Channels::ZERO
    };
    if let Some(frame) = frame {
        let end = Channels {
            alpha: -1.0,
            ..if morph {
                displace(&new, &old)
            } else {
                Channels::ZERO
            }
        };
        push_snapshot(state, frame, Track::leave(leg, end));
        from.alpha = -1.0;
    } else if !morph {
        return;
    }
    state
        .borders
        .anim
        .arrive(&state.space, &window, Track::arrive(leg, from));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::focus::state_tests::{
        client::{Client, Toplevel},
        harness, Harness,
    };
    use smithay::backend::renderer::gles::GlesTexture;
    use smithay::backend::renderer::ContextId;

    fn mapped() -> (Harness, Client, Toplevel, Window) {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let t = c.create_toplevel(&mut h);
        c.commit(&mut h, &t.surface);
        c.attach(&mut h, &t.surface);
        let w = h.state.space.elements().next().cloned().expect("mapped");
        (h, c, t, w)
    }

    fn listed(h: &mut Harness) -> usize {
        crate::ipc::methods::dispatch(&mut h.state, 0, "get_windows", &serde_json::json!({}))
            .unwrap_or_else(|_| panic!("get_windows"))
            .as_array()
            .expect("array")
            .len()
    }

    fn centre_of(g: Rectangle<i32, Logical>) -> Point<f64, Logical> {
        let c = centre(&g);
        Point::from((c.0, c.1))
    }

    /// The frame `capture` would take, minus textures (there is no GL context
    /// in a unit test): enough to drive the ghost's life cycle.
    fn frame(h: &Harness, w: &Window) -> Frame {
        let geometry = h.state.space.element_geometry(w).expect("mapped");
        Frame {
            surfaces: Vec::new(),
            output: output_for(&h.state, &geometry).expect("output"),
            geometry,
            active: false,
        }
    }

    /// COMP-02 §11: a track in flight never moves what the shell reports.
    #[test]
    fn geometry_and_listing_are_the_target_mid_animation() {
        let (mut h, _c, _t, w) = mapped();
        let target = h.state.space.element_geometry(&w).expect("mapped");
        let leg = Leg::new(Instant::now(), 10_000, crate::config::animations::Curve::Linear);
        h.state.borders.anim.arrive(
            &h.state.space,
            &w,
            Track::arrive(
                leg,
                Channels {
                    dx: 500.0,
                    dy: -300.0,
                    sx: -0.5,
                    sy: -0.5,
                    alpha: -1.0,
                },
            ),
        );
        assert!(h.state.borders.anim.running());
        assert_eq!(h.state.space.element_geometry(&w), Some(target));
        assert_eq!(listed(&mut h), 1);
        let hit = crate::shell::surface_under(&h.state, centre_of(target));
        assert_eq!(hit.map(|(s, _)| s), crate::shell::window_surface(&w));
    }

    /// A ghost is not in `space`: not listed, not hit-tested.
    #[test]
    fn ghosts_are_neither_listed_nor_hit_tested() {
        let (mut h, _c, _t, w) = mapped();
        let geo = h.state.space.element_geometry(&w).expect("mapped");
        crate::shell::switch_workspace(&mut h.state, 2);
        assert_eq!(h.state.borders.anim.ghosts.len(), 1, "outgoing half is a ghost");
        assert!(h.state.borders.anim.running());
        assert_eq!(h.state.space.elements().count(), 0);
        assert_eq!(
            listed(&mut h),
            0,
            "an inactive workspace is not listed, ghost or not"
        );
        assert!(crate::shell::surface_under(&h.state, centre_of(geo)).is_none());

        // Back on the first workspace the mapped copy wins: no double draw.
        crate::shell::switch_workspace(&mut h.state, 1);
        let anims = h.state.config.animations.clone();
        h.state.borders.anim.sync(&h.state.space, &anims, None);
        assert!(h.state.borders.anim.ghosts.is_empty());
    }

    #[test]
    fn minimize_leaves_a_ghost_and_unminimize_arrives() {
        let (mut h, _c, _t, w) = mapped();
        crate::shell::minimize_window(&mut h.state, &w);
        assert_eq!(h.state.borders.anim.ghosts.len(), 1);
        assert_eq!(h.state.space.elements().count(), 0);
        assert!(crate::shell::surface_under(&h.state, Point::from((400.0, 300.0))).is_none());
        crate::shell::unminimize_window(&mut h.state, &w);
        assert!(h.state.space.element_geometry(&w).is_some());
        assert!(h.state.borders.anim.running());
    }

    /// The close ghost owns its textures, so neither the null-buffer commit
    /// that drops the client's buffer nor the destroy that follows takes it.
    #[test]
    fn a_close_snapshot_survives_a_null_commit_and_a_destroy() {
        let (mut h, mut c, t, w) = mapped();
        // A context with nothing imported for it: `capture` finds no texture
        // and pushes nothing, rather than panicking.
        h.state.borders.anim.context = Some(ContextId::<GlesTexture>::new());
        close(&mut h.state, &w);
        assert!(h.state.borders.anim.ghosts.is_empty());

        let f = frame(&h, &w);
        close_frame(&mut h.state, f);
        assert_eq!(h.state.borders.anim.ghosts.len(), 1);

        t.surface.attach(None, 0, 0);
        c.commit(&mut h, &t.surface);
        assert_eq!(h.state.space.elements().count(), 0, "unmapped by the null buffer");
        assert_eq!(h.state.borders.anim.ghosts.len(), 1, "survives the null commit");

        t.toplevel.destroy();
        t.xdg.destroy();
        t.surface.destroy();
        c.pump(&mut h);
        assert_eq!(h.state.borders.anim.ghosts.len(), 1, "survives the destroy");
        assert!(h.state.borders.anim.running());

        // It plays out and is dropped.
        let anims = h.state.config.animations.clone();
        h.state.borders.anim.sync_at(
            &h.state.space,
            &anims,
            None,
            Instant::now() + std::time::Duration::from_secs(5),
        );
        assert!(h.state.borders.anim.ghosts.is_empty());
        assert!(!h.state.borders.anim.running());
    }

    /// A client that dies while its window is a live ghost: no panic, and the
    /// ghost is dropped rather than drawn from a dead surface.
    #[test]
    fn a_client_destroyed_mid_animation_does_not_panic() {
        let (mut h, mut c, t, w) = mapped();
        crate::shell::minimize_window(&mut h.state, &w);
        assert_eq!(h.state.borders.anim.ghosts.len(), 1);
        t.toplevel.destroy();
        t.xdg.destroy();
        t.surface.destroy();
        c.pump(&mut h);
        let anims = h.state.config.animations.clone();
        h.state.borders.anim.sync(&h.state.space, &anims, None);
        assert!(h.state.borders.anim.ghosts.is_empty());
        // And a close on a window whose surface is gone does nothing.
        close(&mut h.state, &w);
        assert!(h.state.borders.anim.ghosts.is_empty());
    }

    /// With the event off nothing is pushed.
    #[test]
    fn an_off_event_pushes_nothing() {
        let (mut h, _c, _t, w) = mapped();
        h.state.config.animations.preset = crate::config::animations::Preset::Off;
        crate::shell::minimize_window(&mut h.state, &w);
        crate::shell::switch_workspace(&mut h.state, 2);
        assert!(h.state.borders.anim.ghosts.is_empty());
    }
}
