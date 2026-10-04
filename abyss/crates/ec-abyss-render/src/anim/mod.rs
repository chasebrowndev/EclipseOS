// SPDX-License-Identifier: AGPL-3.0-only
//! Render-only window animations (COMP-02 §9).
//!
//! The shell always maps a window at its *target* geometry, so `scene` and
//! `get_tree` report where a window is going, never where it is mid-flight —
//! an agent can never click at an interpolated position. Animation lives
//! entirely in this store: it remembers each window's previous target and
//! hands the render path a [`Transform`] (offset, scale, alpha) to draw it
//! with. `capture.rs` builds its own pass from `space` and never reads this
//! store, so capture never sees animation.
//!
//! What runs is resolved per event from `animations` in the config
//! (`ec_abyss_config::animations`): `window-move` glides or morphs a window
//! whose target changed, `window-open` pops, fades, slides or zooms a window
//! seen for the first time, `focus` crossfades the border colour, and
//! `workspace-switch` slides (or fades) the arriving workspace's windows in
//! through [`AnimStore::slide`]. Windows that have already left the space —
//! closed, minimized, on the outgoing workspace — are drawn as [`Ghost`]s.
//!
//! With no track and no ghost, every window is drawn exactly as it would be
//! with animation off: unwrapped, so damage tracking and direct scanout are
//! untouched, and [`AnimStore::running`] is false so an idle compositor
//! renders nothing.

pub mod curve;
pub mod ghost;
pub mod layer;
pub mod shader;
pub mod shed;
pub mod track;

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Instant;

use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::ContextId;
use smithay::desktop::space::SpaceElement;
use smithay::desktop::{LayerSurface, Space, Window};
use smithay::output::Output;
use smithay::utils::{Logical, Rectangle, Size};

use ec_abyss_config::animations::Event;
use ec_abyss_config::Animations;

pub use curve::{Curve, Leg};
pub use ghost::{snapshot, Ghost, SnapshotSurface};
pub use layer::layer_motion;
pub use shader::{Registry as ShaderRegistry, RunKind, ShaderElement, ShaderRun};
pub use shed::{ShedLevel, ShedMonitor};
pub use track::{Channels, ScaledElement, Track, Transform};

/// What `ev` resolves to right now: the leg to run and the built-in style to
/// run it in, or `None` when it is off.
///
/// A style this build does not know — an add-on `pack:style` whose pack is
/// missing, or not drawn by the engine yet — falls back to the event's first
/// built-in style, which is never `none`.
///
/// `shed` is the frame-budget level (COMP-14 §6): `FadeOnly` turns every
/// motion style into a short fade (events with no fade, like a move, stop), and
/// `Off` resolves everything to none. `BuiltinOnly` is the fallback above,
/// which is all the engine has until add-on styles are drawn.
pub fn resolve(anims: &Animations, ev: Event, now: Instant, shed: ShedLevel) -> Option<(Leg, &'static str)> {
    if shed == ShedLevel::Off {
        return None;
    }
    let r = anims.resolve(ev);
    if r.off() {
        return None;
    }
    let styles = ev.styles();
    let style = styles
        .iter()
        .copied()
        .find(|s| *s == r.style)
        .unwrap_or(styles[0]);
    if style == "none" {
        return None;
    }
    if shed >= ShedLevel::FadeOnly && !matches!(style, "fade" | "crossfade") {
        return ev.is_builtin("fade").then(|| {
            (
                Leg::new(
                    now,
                    r.duration_ms.min(100),
                    ec_abyss_config::animations::Curve::EaseOut,
                ),
                "fade",
            )
        });
    }
    Some((Leg::new(now, r.duration_ms, r.curve), style))
}

/// How a `window-open` style starts, as a displacement from the identity.
fn open_from(style: &str) -> Channels {
    let fade = Channels {
        alpha: -1.0,
        ..Channels::ZERO
    };
    match style {
        "fade" => fade,
        // Rises 24 logical px into place.
        "slide" => Channels { dy: 24.0, ..fade },
        "zoom" => Channels {
            sx: -0.4,
            sy: -0.4,
            ..fade
        },
        // "pop", and the fallback.
        _ => Channels {
            sx: -0.08,
            sy: -0.08,
            ..fade
        },
    }
}

/// How a `window-close` style ends, as a displacement from the identity:
/// closing mirrors opening, so `pop` shrinks to 0.92, `slide` sinks 24 px,
/// and every style fades out.
pub fn close_to(style: &str) -> Channels {
    open_from(style)
}

/// One window's running tracks.
#[derive(Debug, Default)]
struct Tracks {
    /// `window-move`, and the arriving half of `workspace-switch`.
    motion: Option<Track>,
    /// `window-open`, and `workspace-switch` `fade`.
    open: Option<Track>,
    /// An add-on shader playing `window-open` in place of `open` once its
    /// program is compiled. `open` runs beside it as the fallback.
    shader: Option<ShaderRun>,
    /// `focus`: the crossfade, and the focus state it is headed towards.
    focus: Option<(Leg, bool)>,
}

impl Tracks {
    fn retire(&mut self, now: Instant) {
        if self.motion.is_some_and(|t| t.done(now)) {
            self.motion = None;
        }
        if self.open.is_some_and(|t| t.done(now)) {
            self.open = None;
        }
        if self.shader.as_ref().is_some_and(|r| r.done(now)) {
            self.shader = None;
        }
        if self.focus.is_some_and(|(l, _)| l.done(now)) {
            self.focus = None;
        }
    }

    fn idle(&self) -> bool {
        self.motion.is_none() && self.open.is_none() && self.shader.is_none() && self.focus.is_none()
    }
}

/// Per-window animation state, kept between frames.
#[derive(Debug)]
pub struct AnimStore<W = Window> {
    tracks: HashMap<W, Tracks>,
    /// Last target geometry seen for each live window, animating or not.
    targets: HashMap<W, Rectangle<i32, Logical>>,
    /// Focus state each window was last drawn with, animating or not.
    focused: HashMap<W, bool>,
    /// Windows that have left the space, still playing out.
    pub ghosts: Vec<Ghost<W>>,
    /// The renderer context the last frame was drawn with, so the shell can
    /// [`snapshot`] a closing window's textures from outside the render path.
    /// `None` until the first frame, when nothing has a texture to keep.
    pub context: Option<ContextId<GlesTexture>>,
    /// `layer-open` per layer surface and output: seen once it has a buffer,
    /// and running while it holds a track (see [`layer`]).
    layers: HashMap<(Output, LayerSurface), Option<Track>>,
    /// The frame-budget level every event resolves under.
    shed: ShedLevel,
    /// The clock this frame is drawn at, set by `sync`, so every query in one
    /// frame (and every output of it) agrees.
    now: Instant,
    running: bool,
    /// Add-on transition shaders (ADR 0071): the installed styles and their
    /// compiled programs. Empty while the `transition-shaders` hook is off.
    pub shaders: ShaderRegistry,
}

impl<W> Default for AnimStore<W> {
    fn default() -> Self {
        Self {
            tracks: HashMap::new(),
            targets: HashMap::new(),
            focused: HashMap::new(),
            ghosts: Vec::new(),
            context: None,
            layers: HashMap::new(),
            shed: ShedLevel::Full,
            now: Instant::now(),
            running: false,
            shaders: ShaderRegistry::default(),
        }
    }
}

impl<W: SpaceElement + Clone + Eq + Hash> AnimStore<W> {
    /// True while any window is animating or any ghost is playing out. The
    /// backends keep repainting for as long as this holds and stop the moment
    /// it clears, so an idle compositor is idle (COMP-02 §9).
    pub fn running(&self) -> bool {
        self.running
    }

    /// Set the shedding level from the frame-budget monitor. Applies to events
    /// that start after this; what is already running finishes.
    pub fn set_shed(&mut self, shed: ShedLevel) {
        self.shed = shed;
    }

    pub fn shed(&self) -> ShedLevel {
        self.shed
    }

    /// Note this frame's targets, start what changed, retire what finished.
    ///
    /// Cheap and idempotent: a multi-output frame calls it once per output and
    /// gets the same answer, because a track is only started when a target
    /// *changes* and everything else is derived from the clock. Events are
    /// resolved only when one fires, never per frame.
    pub fn sync(&mut self, space: &Space<W>, anims: &Animations, focus: Option<&W>) {
        self.sync_at(space, anims, focus, Instant::now());
    }

    /// [`AnimStore::sync`] at a given instant.
    pub fn sync_at(&mut self, space: &Space<W>, anims: &Animations, focus: Option<&W>, now: Instant) {
        self.now = now;
        // No per-frame allocation: membership is a scan of the space, which
        // holds a handful of windows.
        let alive = |w: &W| space.elements().any(|l| l == w);
        self.targets.retain(|w, _| alive(w));
        self.tracks.retain(|w, _| alive(w));
        self.focused.retain(|w, _| alive(w));
        // A live ghost whose window is back in the space (switched back to
        // mid-slide) would be drawn twice; the mapped copy wins. One whose
        // client died has nothing left to draw.
        self.ghosts
            .retain(|g| !g.done(now) && !g.window().is_some_and(|w| alive(w) || !w.alive()));

        // Resolved on first use this frame, at most once per event.
        let shed = self.shed;
        let mut open: Option<Option<(Leg, &'static str)>> = None;
        let mut motion: Option<Option<(Leg, &'static str)>> = None;
        let mut crossfade: Option<Option<(Leg, &'static str)>> = None;

        for window in space.elements() {
            let Some(target) = space.element_geometry(window) else {
                continue;
            };
            let target = &target;
            // `focus`: crossfade whenever a window's focus state flips. A
            // window seen for the first time takes its colour immediately.
            let active = focus == Some(window);
            if let Some(was) = self.focused.insert(window.clone(), active) {
                if was != active {
                    if let Some((leg, _)) =
                        *crossfade.get_or_insert_with(|| resolve(anims, Event::Focus, now, shed))
                    {
                        self.tracks.entry(window.clone()).or_default().focus = Some((leg, active));
                    }
                }
            }

            match self.targets.insert(window.clone(), *target) {
                // A window that was already here has not moved.
                Some(previous) if previous == *target => {}
                // First sighting: `window-open`. It appears at its target
                // rather than flying in from the origin.
                None => {
                    if let Some((leg, style)) =
                        *open.get_or_insert_with(|| resolve(anims, Event::WindowOpen, now, shed))
                    {
                        // An add-on style sets the run's length; the built-in
                        // it falls back to runs on the same leg.
                        let run = self
                            .shaders
                            .start(anims, RunKind::Open, now, shed, (0.0, 0.0), 0.0);
                        let leg = run.as_ref().map_or(leg, |r| r.leg);
                        let tracks = self.tracks.entry(window.clone()).or_default();
                        tracks.open = Some(Track::arrive(leg, open_from(style)));
                        tracks.shader = run;
                    }
                }
                // `window-move`. Off: never start a move from a target change;
                // one already in flight (a workspace slide) finishes.
                Some(previous) => {
                    let Some((leg, style)) =
                        *motion.get_or_insert_with(|| resolve(anims, Event::WindowMove, now, shed))
                    else {
                        continue;
                    };
                    let (shift, ratio) = match style {
                        // Position and size: the window's centre travels,
                        // and the live buffer scales from the old size to
                        // the new.
                        "morph" => {
                            let centre = |r: &Rectangle<i32, Logical>| {
                                (
                                    r.loc.x as f64 + r.size.w as f64 / 2.0,
                                    r.loc.y as f64 + r.size.h as f64 / 2.0,
                                )
                            };
                            let (from, to) = (centre(&previous), centre(target));
                            let ratio = |a: i32, b: i32| if b > 0 { a as f64 / b as f64 } else { 1.0 };
                            (
                                (from.0 - to.0, from.1 - to.1),
                                (
                                    ratio(previous.size.w, target.size.w),
                                    ratio(previous.size.h, target.size.h),
                                ),
                            )
                        }
                        // "glide": position only; a resize lands at once.
                        _ => (
                            (
                                (previous.loc.x - target.loc.x) as f64,
                                (previous.loc.y - target.loc.y) as f64,
                            ),
                            (1.0, 1.0),
                        ),
                    };
                    if shift == (0.0, 0.0) && ratio == (1.0, 1.0) {
                        continue;
                    }
                    let tracks = self.tracks.entry(window.clone()).or_default();
                    match &mut tracks.motion {
                        // Carry on from where it is drawn, at the speed it is
                        // moving, so a move retargeted mid-flight never jumps.
                        Some(track) => track.retarget(leg, shift, ratio, now),
                        None => {
                            let mut track = Track::arrive(leg, Channels::ZERO);
                            track.retarget(leg, shift, ratio, now);
                            tracks.motion = Some(track);
                        }
                    }
                }
            }
        }

        for tracks in self.tracks.values_mut() {
            tracks.retire(now);
        }
        self.tracks.retain(|_, t| !t.idle());
        self.retire_layers(now);
        self.running = !self.tracks.is_empty() || !self.ghosts.is_empty() || self.layers_running();
    }

    /// Mark `windows` as already shown at their current targets, so the next
    /// `sync` starts no `window-open` for them. For a window that was mapped
    /// again rather than opened: a workspace switch, an unminimize.
    pub fn settle(&mut self, space: &Space<W>, windows: &[W]) {
        for window in windows {
            if let Some(target) = space.element_geometry(window) {
                self.targets.insert(window.clone(), target);
            }
        }
    }

    /// `workspace-switch`, arriving half: slide `windows` in from the side the
    /// switch came from (`dir` +1 = from the right/bottom, −1 = left/top),
    /// across an output of `output` logical size, or fade them in.
    ///
    /// The windows are mapped at their targets first, so nothing outside the
    /// render path sees the offset (COMP-02 §9). With the event off they are
    /// still settled, so they appear without a `window-open`.
    pub fn slide(
        &mut self,
        space: &Space<W>,
        anims: &Animations,
        windows: &[W],
        output: Size<i32, Logical>,
        dir: i32,
    ) {
        self.settle(space, windows);
        let now = Instant::now();
        let Some((leg, style)) = resolve(anims, Event::WorkspaceSwitch, now, self.shed) else {
            return;
        };
        for window in windows {
            if space.element_geometry(window).is_none() {
                continue;
            }
            let tracks = self.tracks.entry(window.clone()).or_default();
            match style {
                "fade" => {
                    tracks.open = Some(Track::arrive(
                        leg,
                        Channels {
                            alpha: -1.0,
                            ..Channels::ZERO
                        },
                    ));
                }
                "slide-vertical" => {
                    tracks.motion = Some(Track::arrive(
                        leg,
                        Channels {
                            dy: (dir * output.h) as f64,
                            ..Channels::ZERO
                        },
                    ));
                }
                // "slide", and the fallback.
                _ => {
                    tracks.motion = Some(Track::arrive(
                        leg,
                        Channels {
                            dx: (dir * output.w) as f64,
                            ..Channels::ZERO
                        },
                    ));
                }
            }
        }
        self.running |= !self.tracks.is_empty();
    }

    /// Start `track` on `window` in place of whatever it was running, and
    /// settle it so the next `sync` starts no `window-open` or `window-move`
    /// for the change that caused it. For a window that came back or changed
    /// state under an event of its own: unminimize, carry to a workspace, a
    /// fullscreen crossfade. A window not in the space is skipped.
    pub fn arrive(&mut self, space: &Space<W>, window: &W, track: Track) {
        if space.element_geometry(window).is_none() {
            return;
        }
        self.settle(space, std::slice::from_ref(window));
        let tracks = self.tracks.entry(window.clone()).or_default();
        tracks.motion = Some(track);
        tracks.open = None;
        self.running = true;
    }

    /// Hand a window that has left the space to the render path. It counts
    /// toward [`AnimStore::running`] until its track finishes.
    pub fn push_ghost(&mut self, ghost: Ghost<W>) {
        self.ghosts.push(ghost);
        self.running = true;
    }

    /// How to draw `window` this frame. The identity for a window with
    /// nothing running.
    pub fn transform(&self, window: &W) -> Transform {
        let Some(tracks) = self.tracks.get(window) else {
            return Transform::identity();
        };
        let mut t = Transform::identity();
        // A shader that is drawing the open owns it; the built-in open track
        // only runs when the shader cannot.
        let open = if self.window_shader(window).is_some() {
            None
        } else {
            tracks.open
        };
        for track in [tracks.motion, open].into_iter().flatten() {
            t = t.compose(track.at(self.now).0);
        }
        t
    }

    /// A ghost's transform this frame, relative to where it left from.
    pub fn ghost_transform(&self, ghost: &Ghost<W>) -> Transform {
        if self.ghost_shader(ghost).is_some() {
            return Transform::identity();
        }
        Transform::identity().compose(ghost.track().at(self.now).0)
    }

    /// The shader run drawing `window`'s open this frame: only once its
    /// program is compiled and the style still usable.
    pub fn window_shader(&self, window: &W) -> Option<&ShaderRun> {
        let run = self.tracks.get(window)?.shader.as_ref()?;
        self.shaders.ready(&run.style).then_some(run)
    }

    /// The shader run drawing `ghost` this frame, on the same terms.
    pub fn ghost_shader<'g>(&self, ghost: &'g Ghost<W>) -> Option<&'g ShaderRun> {
        let run = ghost.shader()?;
        self.shaders.ready(&run.style).then_some(run)
    }

    /// Compile what this frame's runs need, once per style, and drop the
    /// offscreen copies of runs that finished. Call after `sync`, before any
    /// query. A no-op with no catalog.
    pub fn prepare_shaders(&mut self, renderer: &mut GlesRenderer) {
        if self.shaders.styles().is_empty() {
            return;
        }
        let mut live = std::collections::HashSet::new();
        for run in self
            .tracks
            .values()
            .filter_map(|t| t.shader.as_ref())
            .chain(self.ghosts.iter().filter_map(|g| g.shader()))
        {
            self.shaders.ensure(renderer, &run.style);
            live.insert(run.serial);
        }
        self.shaders.retain_textures(&live);
    }

    /// The instant this frame is drawn at.
    pub fn now(&self) -> Instant {
        self.now
    }

    /// The border colour to draw, crossfading on focus change.
    pub fn border_color(&self, window: &W, active: [f32; 4], inactive: [f32; 4]) -> [f32; 4] {
        let crossfade = self.tracks.get(window).and_then(|t| t.focus);
        let (from, to, t) = match crossfade {
            // `towards` is where the focus went; the fade runs from the other.
            Some((leg, true)) => (inactive, active, leg.progress(self.now) as f32),
            Some((leg, false)) => (active, inactive, leg.progress(self.now) as f32),
            None if self.focused.get(window).copied().unwrap_or(false) => return active,
            None => return inactive,
        };
        std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use ec_abyss_config::animations::Preset;
    use smithay::output::Output;
    use smithay::utils::Point;

    /// A stand-in toplevel whose size the test can change: `Window` needs a
    /// live client, the store does not.
    #[derive(Clone, Debug)]
    struct Fake(u32, Rc<Cell<(i32, i32)>>);
    impl Fake {
        fn new(id: u32) -> Self {
            Fake(id, Rc::new(Cell::new((300, 200))))
        }
    }
    impl PartialEq for Fake {
        fn eq(&self, other: &Self) -> bool {
            self.0 == other.0
        }
    }
    impl Eq for Fake {}
    impl Hash for Fake {
        fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
            self.0.hash(h)
        }
    }
    impl smithay::utils::IsAlive for Fake {
        fn alive(&self) -> bool {
            true
        }
    }
    impl SpaceElement for Fake {
        fn bbox(&self) -> Rectangle<i32, Logical> {
            Rectangle::new((0, 0).into(), self.1.get().into())
        }
        fn is_in_input_region(&self, _: &Point<f64, Logical>) -> bool {
            true
        }
        fn set_activate(&self, _: bool) {}
        fn output_enter(&self, _: &Output, _: Rectangle<i32, Logical>) {}
        fn output_leave(&self, _: &Output) {}
    }

    fn anims(preset: Preset) -> Animations {
        Animations {
            preset,
            speed: 1.0,
            reduce_motion: false,
            overrides: Default::default(),
        }
    }

    fn output() -> Output {
        use smithay::output::PhysicalProperties;
        Output::new(
            "A".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "abyss".into(),
                model: "virtual".into(),
            },
        )
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A store that has seen `w` at `at` once, with nothing running.
    fn seen(anims: &Animations, w: &Fake, at: (i32, i32)) -> (AnimStore<Fake>, Space<Fake>, Instant) {
        let mut space = Space::default();
        space.map_element(w.clone(), at, false);
        let mut store = AnimStore::default();
        let t0 = Instant::now();
        store.settle(&space, std::slice::from_ref(w));
        store.sync_at(&space, anims, None, t0);
        (store, space, t0)
    }

    #[test]
    fn preset_off_produces_no_transforms() {
        let off = anims(Preset::Off);
        let w = Fake::new(1);
        let mut space = Space::default();
        let mut store = AnimStore::default();
        let t0 = Instant::now();
        // Open, move, resize, refocus, switch workspace: nothing animates.
        space.map_element(w.clone(), (0, 0), false);
        store.sync_at(&space, &off, None, t0);
        space.map_element(w.clone(), (500, 300), false);
        w.1.set((640, 480));
        store.sync_at(&space, &off, Some(&w), t0 + ms(10));
        store.slide(&space, &off, std::slice::from_ref(&w), (1920, 1080).into(), 1);
        store.sync_at(&space, &off, None, t0 + ms(20));
        assert!(store.transform(&w).is_identity());
        assert!(!store.running());
        assert_eq!(store.border_color(&w, [1.0; 4], [0.0; 4]), [0.0; 4]);
    }

    #[test]
    fn an_opened_window_pops_in_and_then_is_idle() {
        let smooth = anims(Preset::Smooth);
        let w = Fake::new(1);
        let mut space = Space::default();
        let mut store = AnimStore::default();
        let t0 = Instant::now();
        space.map_element(w.clone(), (0, 0), false);
        store.sync_at(&space, &smooth, None, t0);
        assert!(store.running());
        let start = store.transform(&w);
        assert!(start.scaled() && start.alpha < 0.05, "{start:?}");
        // Finished: the identity, nothing running — the plain Surface path.
        store.sync_at(&space, &smooth, None, t0 + ms(1000));
        assert!(!store.running());
        let end = store.transform(&w);
        assert!(end.is_identity() && !end.scaled());
    }

    #[test]
    fn a_moved_window_glides_from_where_it_was() {
        let smooth = anims(Preset::Smooth);
        let w = Fake::new(1);
        let (mut store, mut space, t0) = seen(&smooth, &w, (0, 0));
        assert!(!store.running());
        space.map_element(w.clone(), (400, 0), false);
        store.sync_at(&space, &smooth, None, t0 + ms(10));
        // Drawn where it was: −400 from its new target.
        assert_eq!(store.transform(&w).loc(), Point::from((-400, 0)));
        assert!(!store.transform(&w).scaled());
        store.sync_at(&space, &smooth, None, t0 + ms(1000));
        assert!(!store.running());
        assert!(store.transform(&w).is_identity());
    }

    #[test]
    fn morph_scales_the_live_buffer_from_the_old_size() {
        let mut lively = anims(Preset::Lively);
        lively.overrides.insert(
            Event::WindowMove,
            ec_abyss_config::animations::Override {
                style: Some("morph".into()),
                ..Default::default()
            },
        );
        let w = Fake::new(1);
        let (mut store, space, t0) = seen(&lively, &w, (0, 0));
        // 300×200 → 600×400 at the same top-left.
        w.1.set((600, 400));
        store.sync_at(&space, &lively, None, t0 + ms(10));
        let t = store.transform(&w);
        assert!((t.scale.0 - 0.5).abs() < 1e-9 && (t.scale.1 - 0.5).abs() < 1e-9);
        // Drawn over the old rect.
        let target = Rectangle::new((0, 0).into(), (600, 400).into());
        assert_eq!(
            t.map(target, track::pivot(&t, target)),
            Rectangle::new((0, 0).into(), (300, 200).into())
        );
    }

    #[test]
    fn sync_is_idempotent_across_outputs() {
        let smooth = anims(Preset::Smooth);
        let w = Fake::new(1);
        let (mut store, mut space, t0) = seen(&smooth, &w, (0, 0));
        space.map_element(w.clone(), (400, 0), false);
        let frame = t0 + ms(16);
        // One frame, three outputs: each calls sync before drawing.
        store.sync_at(&space, &smooth, Some(&w), frame);
        let first = (store.transform(&w), store.border_color(&w, [1.0; 4], [0.0; 4]));
        for _ in 0..2 {
            store.sync_at(&space, &smooth, Some(&w), frame);
            assert_eq!(
                (store.transform(&w), store.border_color(&w, [1.0; 4], [0.0; 4])),
                first
            );
        }
        // And the move did start, from where it was drawn.
        assert_eq!(first.0.loc(), Point::from((-400, 0)));
    }

    #[test]
    fn a_retargeted_move_does_not_jump() {
        let smooth = anims(Preset::Smooth);
        let w = Fake::new(1);
        let (mut store, mut space, t0) = seen(&smooth, &w, (0, 0));
        space.map_element(w.clone(), (400, 0), false);
        store.sync_at(&space, &smooth, None, t0);
        let mid = t0 + ms(60);
        store.sync_at(&space, &smooth, None, mid);
        let drawn = 400 + store.transform(&w).loc().x;
        // Retargeted to 1000 in the same instant: drawn at the same place.
        space.map_element(w.clone(), (1000, 0), false);
        store.sync_at(&space, &smooth, None, mid);
        assert!((1000 + store.transform(&w).loc().x - drawn).abs() <= 1);
    }

    #[test]
    fn workspace_slide_comes_in_from_the_side() {
        let smooth = anims(Preset::Smooth);
        let w = Fake::new(1);
        let mut space = Space::default();
        let mut store = AnimStore::default();
        space.map_element(w.clone(), (100, 100), false);
        store.slide(&space, &smooth, std::slice::from_ref(&w), (1920, 1080).into(), -1);
        assert!(store.running());
        store.sync_at(&space, &smooth, None, Instant::now());
        let t = store.transform(&w);
        // Settled, so no `window-open` on top: offset only, opaque.
        assert!(t.loc().x < -1800 && t.alpha == 1.0 && !t.scaled(), "{t:?}");
    }

    #[test]
    fn an_unknown_style_falls_back_to_the_first_built_in() {
        let mut smooth = anims(Preset::Smooth);
        smooth.overrides.insert(
            Event::WindowOpen,
            ec_abyss_config::animations::Override {
                style: Some("embers:dissolve".into()),
                ..Default::default()
            },
        );
        let (_, style) = resolve(&smooth, Event::WindowOpen, Instant::now(), ShedLevel::Full).unwrap();
        assert_eq!(style, Event::WindowOpen.styles()[0]);
    }

    #[test]
    fn focus_crossfades_the_border() {
        let smooth = anims(Preset::Smooth);
        let w = Fake::new(1);
        let (mut store, space, t0) = seen(&smooth, &w, (0, 0));
        store.sync_at(&space, &smooth, Some(&w), t0 + ms(10));
        let mid = store.border_color(&w, [1.0; 4], [0.0; 4])[0];
        assert!(mid < 0.5, "{mid}");
        store.sync_at(&space, &smooth, Some(&w), t0 + ms(1000));
        assert_eq!(store.border_color(&w, [1.0; 4], [0.0; 4]), [1.0; 4]);
        assert!(!store.running());
    }

    #[test]
    fn a_ghost_keeps_running_until_it_expires() {
        let smooth = anims(Preset::Smooth);
        let gone = Fake::new(9);
        let space: Space<Fake> = Space::default();
        let mut store = AnimStore::default();
        let t0 = Instant::now();
        store.sync_at(&space, &smooth, None, t0);
        assert!(!store.running());
        let (leg, _) = resolve(&smooth, Event::WorkspaceSwitch, t0, ShedLevel::Full).unwrap();
        store.push_ghost(Ghost::Live {
            window: gone.clone(),
            output: output(),
            from_loc: (0, 0).into(),
            active: false,
            shader: None,
            track: Track::leave(
                leg,
                Channels {
                    dx: -1920.0,
                    ..Channels::ZERO
                },
            ),
        });
        assert!(store.running());
        store.sync_at(&space, &smooth, None, t0 + ms(100));
        assert!(store.running());
        let t = store.ghost_transform(&store.ghosts[0]);
        assert!(t.loc().x < 0 && t.loc().x > -1920);
        store.sync_at(&space, &smooth, None, t0 + leg.duration + ms(1));
        assert!(!store.running());
        assert!(store.ghosts.is_empty());
    }

    #[test]
    fn a_live_ghost_yields_to_its_remapped_window() {
        let smooth = anims(Preset::Smooth);
        let w = Fake::new(1);
        let mut space = Space::default();
        let mut store = AnimStore::default();
        let t0 = Instant::now();
        let (leg, _) = resolve(&smooth, Event::WorkspaceSwitch, t0, ShedLevel::Full).unwrap();
        store.push_ghost(Ghost::Live {
            window: w.clone(),
            output: output(),
            from_loc: (0, 0).into(),
            active: false,
            shader: None,
            track: Track::leave(leg, Channels::ZERO),
        });
        space.map_element(w.clone(), (0, 0), false);
        store.sync_at(&space, &smooth, None, t0 + ms(5));
        assert!(store.ghosts.is_empty());
    }

    #[test]
    fn shedding_turns_motion_into_fade_and_then_stops() {
        let smooth = anims(Preset::Smooth);
        let now = Instant::now();
        let at = |ev, shed| resolve(&smooth, ev, now, shed);
        // Full and BuiltinOnly resolve as configured.
        for shed in [ShedLevel::Full, ShedLevel::BuiltinOnly] {
            assert_eq!(at(Event::WindowOpen, shed).unwrap().1, "pop");
            assert_eq!(at(Event::WindowMove, shed).unwrap().1, "glide");
        }
        // FadeOnly: styles with a fade become one, short; a move has none.
        let (leg, style) = at(Event::WindowOpen, ShedLevel::FadeOnly).unwrap();
        assert_eq!(style, "fade");
        assert!(leg.duration <= ms(100));
        assert_eq!(at(Event::LayerOpen, ShedLevel::FadeOnly).unwrap().1, "fade");
        assert_eq!(at(Event::WindowMove, ShedLevel::FadeOnly), None);
        // The focus crossfade is already a fade.
        assert_eq!(at(Event::Focus, ShedLevel::FadeOnly).unwrap().1, "crossfade");
        // Off: nothing.
        for ev in Event::ALL {
            assert_eq!(at(ev, ShedLevel::Off), None, "{ev:?}");
        }
    }

    #[test]
    fn a_shed_store_starts_no_open_animation() {
        let smooth = anims(Preset::Smooth);
        let w = Fake::new(1);
        let mut space = Space::default();
        let mut store = AnimStore::default();
        store.set_shed(ShedLevel::Off);
        space.map_element(w.clone(), (0, 0), false);
        store.sync_at(&space, &smooth, None, Instant::now());
        assert!(!store.running() && store.transform(&w).is_identity());
    }
}
