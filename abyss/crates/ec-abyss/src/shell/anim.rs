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

use ec_abyss_render::anim::{self, Channels, Ghost, Leg, RunKind, ShaderRun, SnapshotSurface, Track};

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

/// An add-on transition shader run for `kind`, when the event's style is an
/// installed one and the hook is on; `None` runs the built-in style alone.
fn shader_run(state: &mut AbyssState, kind: RunKind, travel: (f64, f64)) -> Option<ShaderRun> {
    if state.lock.locked {
        return None;
    }
    let shed = state.borders.anim.shed();
    state
        .borders
        .anim
        .shaders
        .start(&state.config.animations, kind, Instant::now(), shed, travel, 0.0)
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
    let mut surfaces = anim::snapshot(context, &surface, Point::from((0, 0)) - window.geometry().loc);
    // A commit since the last frame dropped the textures of the buffer it
    // replaced and nothing has imported the new one (a window that draws its
    // last frame and closes at once): the previous frame beats no animation.
    if let Some(last) = state
        .borders
        .anim
        .remembered(window)
        .filter(|l| l.len() > surfaces.len())
    {
        surfaces = last.clone();
    }
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

fn push_snapshot(
    state: &mut AbyssState,
    frame: Frame,
    track: Track,
    shader: Option<ShaderRun>,
    holds: Option<Window>,
) {
    state.borders.anim.push_ghost(Ghost::Snapshot {
        surfaces: frame.surfaces,
        output: frame.output,
        geometry: frame.geometry,
        active: frame.active,
        track,
        shader,
        holds,
    });
}

/// A window still alive but leaving the space: drawn from its live buffers.
/// Called *before* it is unmapped, while its geometry is still known.
fn push_live(
    state: &mut AbyssState,
    window: &Window,
    output: &Output,
    track: Track,
    shader: Option<ShaderRun>,
) {
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
        shader,
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
        close_frame(state, frame, window);
    }
}

/// [`close`] once the frame is in hand. The ghost owns cloned textures, so it
/// outlives the buffer, the surface and the client.
///
/// The ghost names `window`, so [`super::unmap_window`] can hold its tiled
/// neighbours where they are until it is done.
pub fn close_frame(state: &mut AbyssState, frame: Frame, window: &Window) {
    let Some((leg, style)) = resolve(state, Event::WindowClose) else {
        return;
    };
    // An add-on style sets the length; the built-in it falls back to runs on
    // the same leg.
    let run = shader_run(state, RunKind::Close, (0.0, 0.0));
    let leg = run.as_ref().map_or(leg, |r| r.leg);
    push_snapshot(
        state,
        frame,
        Track::leave(leg, anim::close_to(style)),
        run,
        Some(window.clone()),
    );
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
        push_live(state, window, output, Track::leave(leg, end), None);
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
    push_live(state, window, output, Track::leave(leg, end), None);
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

/// Where `window` shrinks to on `output`: the centre of its taskbar chip when
/// the bar reported one for this output (`set_window_chip_rect`), else
/// [`dock`]. Same space as `dock`: global logical.
fn minimize_target(state: &AbyssState, window: &Window, output: &Output) -> Option<(f64, f64)> {
    let chip = state
        .ipc
        .existing_handle(window)
        .and_then(|h| state.ipc.chip(h))
        .zip(state.space.output_geometry(output))
        .zip(state.outputs.by_output(output))
        .filter(|((c, _), e)| c.output == e.connector)
        .map(|((c, out), _)| {
            (
                out.loc.x as f64 + c.rect.loc.x + c.rect.size.w / 2.0,
                out.loc.y as f64 + c.rect.loc.y + c.rect.size.h / 2.0,
            )
        });
    chip.or_else(|| dock(state, output))
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
    let target = minimize_target(state, window, &output);
    let end = match (style, target) {
        ("shrink", Some(dock)) => shrunk(&geo, dock),
        _ => FADE,
    };
    let travel = target.map_or((0.0, 0.0), |d| {
        let c = centre(&geo);
        (d.0 - c.0, d.1 - c.1)
    });
    let run = shader_run(state, RunKind::Minimize, travel);
    let leg = run.as_ref().map_or(leg, |r| r.leg);
    push_live(state, window, &output, Track::leave(leg, end), run);
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
    let from = match (
        style,
        output_for(state, &geo).and_then(|o| minimize_target(state, window, &o)),
    ) {
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
        push_snapshot(state, frame, Track::leave(leg, end), None, None);
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

    #[test]
    fn minimize_aims_at_the_chip_and_falls_back_to_the_dock() {
        let (mut h, _c, _t, w) = mapped();
        let geo = h.state.space.element_geometry(&w).expect("mapped");
        let output = output_for(&h.state, &geo).expect("output");
        let dock_at = dock(&h.state, &output);
        // No chip rect: today's dock centre.
        assert_eq!(minimize_target(&h.state, &w, &output), dock_at);

        let handle = h.state.ipc.handle_for(&w);
        let connector = h
            .state
            .outputs
            .by_output(&output)
            .expect("entry")
            .connector
            .clone();
        let origin = h.state.space.output_geometry(&output).expect("geometry").loc;
        let rect = Rectangle::new((100.0, 20.0).into(), (40.0, 30.0).into());
        h.state.ipc.set_chip(
            handle,
            Some(crate::ipc::ChipRect {
                output: connector.clone(),
                rect,
            }),
        );
        assert_eq!(
            minimize_target(&h.state, &w, &output),
            Some((origin.x as f64 + 120.0, origin.y as f64 + 35.0))
        );

        // A rect reported for another output is ignored.
        h.state.ipc.set_chip(
            handle,
            Some(crate::ipc::ChipRect {
                output: format!("{connector}-other"),
                rect,
            }),
        );
        assert_eq!(minimize_target(&h.state, &w, &output), dock_at);
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
        close_frame(&mut h.state, f, &w);
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

    fn snapshots(h: &Harness) -> usize {
        h.state
            .borders
            .anim
            .ghosts
            .iter()
            .filter(|g| matches!(g, Ghost::Snapshot { .. }))
            .count()
    }

    /// Two tiled windows; `b` is the second (focused) one.
    fn pair() -> (Harness, Client, Toplevel, Window, Toplevel, Window) {
        let (mut h, mut c, t1, w1) = mapped();
        let t2 = c.create_toplevel(&mut h);
        c.commit(&mut h, &t2.surface);
        c.attach(&mut h, &t2.surface);
        let w2 = h
            .state
            .space
            .elements()
            .find(|w| **w != w1)
            .cloned()
            .expect("second window");
        (h, c, t1, w1, t2, w2)
    }

    fn geo(h: &Harness, w: &Window) -> Rectangle<i32, Logical> {
        h.state.space.element_geometry(w).expect("mapped")
    }

    fn close_with_ghost(h: &mut Harness, w: &Window) {
        let f = frame(h, w);
        close_frame(&mut h.state, f, w);
        crate::shell::unmap_window(&mut h.state, w);
    }

    /// A tiled close keeps the neighbours where they were until the ghost is
    /// done, then retiles; `get_tree`-side state and hit-testing agree.
    #[test]
    fn a_tiled_close_holds_the_neighbours_until_the_ghost_is_done() {
        let (mut h, _c, _t1, w1, _t2, w2) = pair();
        let before = geo(&h, &w1);
        close_with_ghost(&mut h, &w2);
        assert_eq!(h.state.retile_hold, crate::shell::RetileHold::Held);
        assert_eq!(geo(&h, &w1), before, "neighbour keeps its geometry");
        assert_eq!(listed(&mut h), 1, "the closed window is gone from the listing");
        let hit = crate::shell::surface_under(&h.state, centre_of(before));
        assert_eq!(hit.map(|(s, _)| s), crate::shell::window_surface(&w1));

        // Still playing: a tick changes nothing.
        crate::shell::tick_retile_hold(&mut h.state);
        assert_eq!(h.state.retile_hold, crate::shell::RetileHold::Held);
        assert_eq!(geo(&h, &w1), before);

        // Done: the layout runs.
        h.state.borders.anim.ghosts.clear();
        crate::shell::tick_retile_hold(&mut h.state);
        assert_eq!(h.state.retile_hold, crate::shell::RetileHold::None);
        assert_ne!(geo(&h, &w1), before, "retiled into the freed space");
    }

    /// A layout event during the hold ends it and retiles at once.
    #[test]
    fn a_new_map_during_the_hold_retiles_immediately() {
        let (mut h, mut c, _t1, w1, _t2, w2) = pair();
        close_with_ghost(&mut h, &w2);
        assert_eq!(h.state.retile_hold, crate::shell::RetileHold::Held);
        let t3 = c.create_toplevel(&mut h);
        c.commit(&mut h, &t3.surface);
        c.attach(&mut h, &t3.surface);
        assert_eq!(h.state.retile_hold, crate::shell::RetileHold::None);
        assert_eq!(h.state.space.elements().count(), 2);
        // Laid out for two windows again: the new one has a tile of its own.
        let w3 = h
            .state
            .space
            .elements()
            .find(|w| **w != w1)
            .cloned()
            .expect("new");
        assert_ne!(geo(&h, &w3), geo(&h, &w1));
    }

    /// A ghost that never draws expires on the armed-clock grace, and the
    /// hold goes with it.
    #[test]
    fn the_hold_does_not_outlive_a_ghost_that_never_draws() {
        let (mut h, _c, _t1, w1, _t2, w2) = pair();
        close_with_ghost(&mut h, &w2);
        let before = geo(&h, &w1);
        assert_eq!(h.state.retile_hold, crate::shell::RetileHold::Held);
        // Age the ghost past its length and the grace.
        let old = Instant::now() - std::time::Duration::from_secs(30);
        let leg = Leg::new(old, 200, crate::config::animations::Curve::Linear);
        for g in h.state.borders.anim.ghosts.iter_mut() {
            if let Ghost::Snapshot { track, .. } = g {
                track.leg = leg;
            }
        }
        crate::shell::tick_retile_hold(&mut h.state);
        assert_eq!(h.state.retile_hold, crate::shell::RetileHold::None);
        assert_ne!(geo(&h, &w1), before);
    }

    #[test]
    fn reduce_motion_and_off_do_not_hold() {
        for reduce in [true, false] {
            let (mut h, _c, _t1, w1, _t2, w2) = pair();
            let before = geo(&h, &w1);
            if reduce {
                h.state.config.animations.reduce_motion = true;
            } else {
                h.state.config.animations.preset = crate::config::animations::Preset::Off;
            }
            close_with_ghost(&mut h, &w2);
            assert_eq!(h.state.retile_hold, crate::shell::RetileHold::None);
            assert_ne!(geo(&h, &w1), before, "retiled at once (reduce {reduce})");
        }
    }

    #[test]
    fn a_floating_close_does_not_hold() {
        let (mut h, _c, _t1, w1, _t2, w2) = pair();
        let rect = Rectangle::new((50, 50).into(), (200, 150).into());
        crate::shell::place_at(&mut h.state, &w2, rect);
        let before = geo(&h, &w1);
        close_with_ghost(&mut h, &w2);
        assert_eq!(h.state.retile_hold, crate::shell::RetileHold::None);
        assert_eq!(geo(&h, &w1), before, "a float took no tile from it");
        assert_eq!(snapshots(&h), 1, "the ghost still plays");
    }

    /// Workspace switch and window-to-workspace never use `window-close`:
    /// their ghosts are live (the window still exists), never snapshots, and
    /// hold nothing.
    #[test]
    fn workspace_moves_never_play_window_close() {
        let (mut h, _c, _t1, _w1, _t2, _w2) = pair();
        crate::shell::switch_workspace(&mut h.state, 2);
        assert_eq!(snapshots(&h), 0);
        assert_eq!(
            h.state.borders.anim.ghosts.len(),
            2,
            "the outgoing half is live ghosts"
        );
        assert_eq!(h.state.retile_hold, crate::shell::RetileHold::None);

        for follow in [false, true] {
            let (mut h, _c, _t1, _w1, _t2, _w2) = pair();
            h.state.config.general.follow_window_to_workspace = follow;
            crate::shell::move_to_workspace(&mut h.state, 3);
            assert_eq!(snapshots(&h), 0, "follow {follow}");
            assert_eq!(h.state.retile_hold, crate::shell::RetileHold::None);
            if !follow {
                assert_eq!(h.state.borders.anim.ghosts.len(), 1, "send_away is a live ghost");
            }
        }
    }

    /// With `window-close` off the close pushes nothing even while the
    /// workspace events run, and the converse.
    #[test]
    fn each_path_resolves_its_own_event() {
        use crate::config::animations::{Event, Override};
        let off = Override {
            style: Some("none".into()),
            ..Default::default()
        };
        let (mut h, _c, _t, w) = mapped();
        h.state
            .config
            .animations
            .overrides
            .insert(Event::WindowClose, off.clone());
        let f = frame(&h, &w);
        close_frame(&mut h.state, f, &w);
        assert!(h.state.borders.anim.ghosts.is_empty(), "close is off");
        crate::shell::switch_workspace(&mut h.state, 2);
        assert_eq!(
            h.state.borders.anim.ghosts.len(),
            1,
            "workspace-switch still plays"
        );

        let (mut h, _c, _t, w) = mapped();
        h.state
            .config
            .animations
            .overrides
            .insert(Event::WorkspaceSwitch, off);
        crate::shell::switch_workspace(&mut h.state, 2);
        assert!(h.state.borders.anim.ghosts.is_empty(), "workspace-switch is off");
        crate::shell::switch_workspace(&mut h.state, 1);
        let f = frame(&h, &w);
        close_frame(&mut h.state, f, &w);
        assert_eq!(snapshots(&h), 1, "close still plays");
    }
}
