// SPDX-License-Identifier: AGPL-3.0-only
//! Add-on transition shaders (ADR 0073 "Transition shaders"; ADR 0066 hook
//! `transition-shaders`).
//!
//! A style is a GLSL ES 1.00 fragment body shipped by an installed animation
//! pack and read by `ec_abyss_config::transitions`. This module owns what
//! happens next, entirely render-side:
//!
//! - [`Registry`]: the catalog `ec-abyss` hands over while the hook is on, the
//!   lazily compiled program per style, and the reasons a style stops being
//!   used (compile failure, repeated budget overruns, shedding).
//! - [`ShaderRun`]: one play of a style over one window, carried by a window's
//!   track or by a ghost. It sits *beside* the built-in track the event
//!   resolved to, never instead of it: when the shader cannot be drawn for any
//!   reason the built-in style simply runs, which is the fallback.
//! - [`ShaderElement`]: the window's offscreen copy drawn through the
//!   style's program on a quad grown by the style's margin.
//!
//! The shader sees only its own window's pixels. It has one sampler, `tex`,
//! holding the window (and its popups) rendered alone, premultiplied, on a
//! transparent ground; there is no backdrop and no way to name one. Nothing
//! here is reachable from `capture.rs`, which builds its own pass from
//! `space`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{Bind, Color32F, ContextId, Offscreen};
use smithay::utils::{
    Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Scale, Size, Transform as BufferTransform,
};

use ec_abyss_config::animations::{is_addon_style, Animations, Event};
use ec_abyss_config::transitions::{PackPreset, TransitionStyle, DRAWN_EVENTS};

use super::curve::Leg;
use super::shed::ShedLevel;

/// Consecutive frames over budget, with a style drawn in them, before it is
/// off for the rest of the session. Below the shed monitor's own run length
/// (`shed::DOWN_AFTER`), so a heavy shader is dropped before the whole system
/// sheds.
pub const OVERRUN_LIMIT: u32 = 8;

/// The largest offscreen copy, either side, physical px. A window past this is
/// drawn with its built-in style.
const MAX_TEXTURE: i32 = 8192;

/// How long past its own length a run that was never drawn is kept before it
/// is dropped (its clock never armed). Past it the built-in style has long
/// finished and nothing is waiting on the run.
const UNARMED_GRACE: Duration = Duration::from_secs(1);

/// The fixed header every style is appended to. The pack supplies the rest,
/// including `void main()`. Everything a style may read is declared here and
/// nothing else is bound.
///
/// - `tex`: the window alone, premultiplied alpha, on a transparent ground.
///   `v_coords` addresses the whole quad (window plus margin).
/// - `alpha`: 1.0.
/// - `progress`: linear time through the run, 0 to 1.
/// - `eased`: the event's curve applied to `progress`; a spring or bounce may
///   overshoot 1.0.
/// - `direction`: +1 while the window appears, -1 while it disappears. Either
///   way `progress` runs 0 to 1; `shown()` is 0 when hidden and 1 when shown.
/// - `kind`: 0 open, 1 close, 2 minimize.
/// - `size`: the window's size, physical px.
/// - `content`: the window's rectangle inside the quad, as `vec4(x, y, w, h)`
///   in `v_coords` units. The margin is whatever is outside it.
/// - `travel`: physical px the window is going to, from its centre (the dock
///   for minimize), else zero.
/// - `side`: -1 or +1 for the side a workspace switch came from, else 0.
/// - `seed`: a fixed value in 0..1 per run, for noise.
/// - `time`: seconds since the run's first drawn frame.
///
/// Output premultiplied colour to `gl_FragColor`.
pub const HEADER: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform float progress;
uniform float eased;
uniform float direction;
uniform float kind;
uniform vec2 size;
uniform vec4 content;
uniform vec2 travel;
uniform float side;
uniform float seed;
uniform float time;

float shown() { return direction > 0.0 ? eased : 1.0 - eased; }

"#;

/// Names and types of the additional uniforms, in the order [`ShaderRun::uniforms`]
/// writes them.
fn uniform_names() -> [UniformName<'static>; 10] {
    [
        UniformName::new("progress", UniformType::_1f),
        UniformName::new("eased", UniformType::_1f),
        UniformName::new("direction", UniformType::_1f),
        UniformName::new("kind", UniformType::_1f),
        UniformName::new("size", UniformType::_2f),
        UniformName::new("content", UniformType::_4f),
        UniformName::new("travel", UniformType::_2f),
        UniformName::new("side", UniformType::_1f),
        UniformName::new("seed", UniformType::_1f),
        UniformName::new("time", UniformType::_1f),
    ]
}

/// What a run is for. The `kind` uniform is its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Open,
    Close,
    Minimize,
}

impl RunKind {
    fn index(self) -> f32 {
        match self {
            RunKind::Open => 0.0,
            RunKind::Close => 1.0,
            RunKind::Minimize => 2.0,
        }
    }

    fn direction(self) -> f32 {
        match self {
            RunKind::Open => 1.0,
            RunKind::Close | RunKind::Minimize => -1.0,
        }
    }

    pub fn event(self) -> Event {
        match self {
            RunKind::Open => Event::WindowOpen,
            RunKind::Close => Event::WindowClose,
            RunKind::Minimize => Event::Minimize,
        }
    }
}

/// One play of a style over one window.
#[derive(Debug, Clone)]
pub struct ShaderRun {
    /// `pack:style`.
    pub style: Arc<str>,
    pub kind: RunKind,
    /// The run's length and curve. `leg.start` is when it was created; the
    /// clock the shader sees starts at [`ShaderRun::armed`] instead.
    pub leg: Leg,
    /// Logical px the quad is grown by.
    pub margin: u32,
    /// The quad also grows toward `travel` (the style's `reach`).
    pub reach: bool,
    /// Set by the first frame that draws the run; until then it reads as
    /// progress 0. Shared by clones, so the render path's copy stamps the one
    /// the store holds.
    pub armed: Arc<OnceLock<Instant>>,
    /// Where the window is headed, from its centre, logical px.
    pub travel: (f64, f64),
    pub side: f32,
    pub seed: f32,
    /// Unique per run: keys its offscreen texture.
    pub serial: u64,
    /// The element's identity across frames.
    pub id: Id,
}

/// The off-centre quad a run is drawn on: how far it extends past the window
/// on each side, physical px.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quad {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Quad {
    pub fn size(&self, win: Size<i32, Physical>) -> Size<i32, Physical> {
        (win.w + self.left + self.right, win.h + self.top + self.bottom).into()
    }

    /// Where the window's top-left sits inside the quad.
    pub fn origin(&self) -> Point<i32, Physical> {
        (self.left, self.top).into()
    }
}

/// How far a quad extends past a `win`-sized window on each side (left, top,
/// right, bottom), logical px. `margin` everywhere; with `reach`, also enough
/// toward `travel` (from the window's centre) that the target lies at least
/// `margin` inside the quad.
pub fn extents(margin: u32, reach: bool, travel: (f64, f64), win: Size<i32, Logical>) -> [i32; 4] {
    let m = margin as i32;
    if !reach {
        return [m; 4];
    }
    let (w, h) = (win.w as f64 / 2.0, win.h as f64 / 2.0);
    let m_f = margin as f64;
    // Target from the top-left is (w/2 + tx); it must be `m` short of the far
    // edge (w + right) and `m` past the near one (-left).
    let side = |toward: f64, half: f64| (toward - half + m_f).ceil() as i32 + 1;
    [
        m.max(side(-travel.0, w)),
        m.max(side(-travel.1, h)),
        m.max(side(travel.0, w)),
        m.max(side(travel.1, h)),
    ]
}

impl ShaderRun {
    /// The quad for a window of `win` logical px at `scale`.
    pub fn quad(&self, win: Size<i32, Logical>, scale: Scale<f64>) -> Quad {
        let [l, t, r, b] = extents(self.margin, self.reach, self.travel, win);
        Quad {
            left: margin_px(l as u32, scale),
            top: margin_px(t as u32, scale),
            right: margin_px(r as u32, scale),
            bottom: margin_px(b as u32, scale),
        }
    }

    /// Stamp the clock; the first call wins. Returns whether this was it.
    pub fn arm(&self, now: Instant) -> bool {
        self.armed.set(now).is_ok()
    }

    /// The leg as the shader sees it: started at the first drawn frame, or
    /// right now while nothing has drawn it.
    fn live_leg(&self, now: Instant) -> Leg {
        Leg {
            start: self.armed.get().copied().unwrap_or(now),
            ..self.leg
        }
    }

    /// Seconds since the first drawn frame; 0 before it.
    pub fn time(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.live_leg(now).start)
            .as_secs_f32()
    }

    /// Linear time through the run, 0 to 1.
    pub fn progress(&self, now: Instant) -> f32 {
        let total = self.leg.duration.as_secs_f64();
        if total <= 0.0 {
            return 1.0;
        }
        (now.saturating_duration_since(self.live_leg(now).start)
            .as_secs_f64()
            / total)
            .clamp(0.0, 1.0) as f32
    }

    /// The curve applied to [`ShaderRun::progress`]; may overshoot 1.0.
    pub fn eased(&self, now: Instant) -> f32 {
        (1.0 - self.live_leg(now).response(now).pd) as f32
    }

    /// How visible the window is, 0 to 1: what the border and shadow follow.
    pub fn visible(&self, now: Instant) -> f32 {
        let e = self.eased(now).clamp(0.0, 1.0);
        if self.kind.direction() > 0.0 {
            e
        } else {
            1.0 - e
        }
    }

    /// Over: its own length after the first drawn frame. A run that never
    /// drew (the shader was unusable and the built-in played instead) is not
    /// kept for ever: it goes [`UNARMED_GRACE`] after its length from creation.
    pub fn done(&self, now: Instant) -> bool {
        match self.armed.get() {
            Some(_) => self.live_leg(now).done(now),
            None => now.saturating_duration_since(self.leg.start) >= self.leg.duration + UNARMED_GRACE,
        }
    }

    /// The uniform values for one frame. `win` is the window's physical size
    /// and `quad` its quad; `scale` converts `travel`.
    pub fn uniforms(
        &self,
        now: Instant,
        win: Size<i32, Physical>,
        quad: Quad,
        scale: Scale<f64>,
    ) -> Vec<Uniform<'static>> {
        let full = quad.size(win);
        let (qw, qh) = (full.w.max(1) as f32, full.h.max(1) as f32);
        let content = [
            quad.left as f32 / qw,
            quad.top as f32 / qh,
            win.w as f32 / qw,
            win.h as f32 / qh,
        ];
        let travel = [(self.travel.0 * scale.x) as f32, (self.travel.1 * scale.y) as f32];
        let elapsed = self.time(now);
        vec![
            Uniform::new("progress", self.progress(now)),
            Uniform::new("eased", self.eased(now)),
            Uniform::new("direction", self.kind.direction()),
            Uniform::new("kind", self.kind.index()),
            Uniform::new("size", [win.w as f32, win.h as f32]),
            Uniform::new("content", content),
            Uniform::new("travel", travel),
            Uniform::new("side", self.side),
            Uniform::new("seed", self.seed),
            Uniform::new("time", elapsed),
        ]
    }
}

/// The margin in physical px at `scale`.
pub fn margin_px(margin: u32, scale: Scale<f64>) -> i32 {
    (margin as f64 * scale.x.max(scale.y)).round() as i32
}

#[derive(Debug)]
enum Slot {
    Ready(GlesTexProgram),
    Failed,
}

/// The installed styles, their programs, and what has been switched off.
#[derive(Debug, Default)]
pub struct Registry {
    styles: Vec<TransitionStyle>,
    presets: Vec<PackPreset>,
    programs: HashMap<String, Slot>,
    /// Styles already warned about, so a missing pack logs once.
    warned: HashSet<String>,
    /// Off for the session: too slow.
    disabled: HashSet<String>,
    overruns: HashMap<String, u32>,
    /// Styles drawn since the last [`Registry::frame_result`].
    drawn: Vec<Arc<str>>,
    textures: HashMap<u64, GlesTexture>,
    /// Open runs keep their tracker, so an unchanged window is not re-drawn
    /// into its copy.
    trackers: HashMap<u64, OutputDamageTracker>,
    /// Serials whose copy holds a finished render of a static ghost.
    rendered: HashSet<u64>,
    /// Add-on styles the resolved config selects, and every pack preset's:
    /// compiled ahead of any run.
    wanted: HashSet<String>,
    /// The config `wanted` was computed from.
    selected: Option<Animations>,
    selection_stale: bool,
    /// A program was compiled since the last [`Registry::frame_result`]: that
    /// frame is not held against any style.
    compiled: bool,
    serial: u64,
    frame: usize,
}

/// The add-on styles `anims` selects for the events a shader is drawn for,
/// plus every style a pack preset names (so picking one is instant).
pub fn wanted_styles(anims: &Animations, presets: &[PackPreset]) -> HashSet<String> {
    let mut out: HashSet<String> = presets
        .iter()
        .flat_map(|p| p.styles.iter().map(|(_, id)| id.clone()))
        .collect();
    for ev in DRAWN_EVENTS {
        let r = anims.resolve(ev);
        if !r.off() && is_addon_style(&r.style) {
            out.insert(r.style);
        }
    }
    out
}

impl Registry {
    /// Install the catalog the loader read (empty while the hook is off). A
    /// style whose source changed is recompiled; one that went away stops
    /// being drawn at once.
    pub fn set_catalog(&mut self, styles: Vec<TransitionStyle>) {
        let keep: HashSet<&str> = styles.iter().map(|s| s.id.as_str()).collect();
        self.programs.retain(|id, _| {
            keep.contains(id.as_str())
                && self
                    .styles
                    .iter()
                    .find(|s| &s.id == id)
                    .zip(styles.iter().find(|s| &s.id == id))
                    .is_some_and(|(a, b)| a.source == b.source)
        });
        self.disabled.retain(|id| keep.contains(id.as_str()));
        self.overruns.retain(|id, _| keep.contains(id.as_str()));
        self.warned.clear();
        self.styles = styles;
        self.selection_stale = true;
    }

    /// Install the pack presets the loader read (empty while the hook is off).
    pub fn set_presets(&mut self, presets: Vec<PackPreset>) {
        self.presets = presets;
        self.selection_stale = true;
    }

    /// Learn which styles `anims` selects. Cheap when nothing changed; call
    /// every frame before [`Registry::compile_wanted`].
    pub fn select(&mut self, anims: &Animations) {
        if !self.selection_stale && self.selected.as_ref() == Some(anims) {
            return;
        }
        self.wanted = wanted_styles(anims, &self.presets);
        self.selected = Some(anims.clone());
        self.selection_stale = false;
    }

    pub fn wanted(&self) -> &HashSet<String> {
        &self.wanted
    }

    /// Compile every wanted style not yet tried, whether or not a run exists,
    /// so no run compiles mid-animation.
    pub fn compile_wanted(&mut self, renderer: &mut GlesRenderer) {
        let todo: Vec<String> = self
            .wanted
            .iter()
            .filter(|id| !self.programs.contains_key(*id) && self.usable(id))
            .cloned()
            .collect();
        for id in todo {
            self.ensure(renderer, &id);
        }
    }

    pub fn presets(&self) -> &[PackPreset] {
        &self.presets
    }

    pub fn styles(&self) -> &[TransitionStyle] {
        &self.styles
    }

    pub fn get(&self, id: &str) -> Option<&TransitionStyle> {
        self.styles.iter().find(|s| s.id == id)
    }

    /// Whether `id` is on: installed, not disabled, not failed to compile.
    fn usable(&self, id: &str) -> bool {
        self.get(id).is_some()
            && !self.disabled.contains(id)
            && !matches!(self.programs.get(id), Some(Slot::Failed))
    }

    /// Whether `id` has a compiled program and may be drawn now. A transform
    /// hands the window to the shader only when this holds.
    pub fn ready(&self, id: &str) -> bool {
        self.usable(id) && matches!(self.programs.get(id), Some(Slot::Ready(_)))
    }

    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.contains(id)
    }

    /// Begin a run of the style `anims` names for `ev`, or `None`: the event's
    /// style is built in, shedding is at `BuiltinOnly` or below, the style
    /// is not installed (warned once, not an error), does not serve the event,
    /// or was disabled. In every `None` case the caller's built-in style runs.
    ///
    /// The run's duration and curve are the user's override if there is one,
    /// else the style's own, then `speed`.
    pub fn start(
        &mut self,
        anims: &Animations,
        kind: RunKind,
        now: Instant,
        shed: ShedLevel,
        travel: (f64, f64),
        side: f32,
    ) -> Option<ShaderRun> {
        if shed >= ShedLevel::BuiltinOnly {
            return None;
        }
        let ev = kind.event();
        let r = anims.resolve(ev);
        if r.off() || !is_addon_style(&r.style) {
            return None;
        }
        let Some(style) = self.get(&r.style).filter(|s| s.serves(ev)) else {
            if self.warned.insert(r.style.clone()) {
                tracing::warn!(
                    style = r.style,
                    event = ev.key(),
                    "transition style is not installed for this event; using the built-in"
                );
            }
            return None;
        };
        if !self.usable(&r.style) {
            return None;
        }
        let over = anims.overrides.get(&ev);
        let speed = if anims.speed > 0.0 { anims.speed } else { 1.0 };
        let ms =
            (over.and_then(|o| o.duration_ms).unwrap_or(style.duration_ms) as f64 / speed).round() as u32;
        let curve = over.and_then(|o| o.curve).unwrap_or(style.curve);
        let (margin, reach) = (style.margin, style.reach);
        self.serial += 1;
        let serial = self.serial;
        Some(ShaderRun {
            style: Arc::from(r.style.as_str()),
            kind,
            leg: Leg::new(now, ms, curve),
            margin,
            reach,
            armed: Arc::new(OnceLock::new()),
            travel,
            side,
            seed: ((serial.wrapping_mul(2_654_435_761) >> 4) % 1000) as f32 / 1000.0,
            serial,
            id: Id::new(),
        })
    }

    /// Compile `id` if it has not been tried. A failure is final for the
    /// session and logged once; the built-in style runs in its place.
    pub fn ensure(&mut self, renderer: &mut GlesRenderer, id: &str) {
        if self.programs.contains_key(id) || !self.usable(id) {
            return;
        }
        let Some(style) = self.get(id) else { return };
        let source = format!("{HEADER}{}", style.source);
        let slot = match renderer.compile_custom_texture_shader(source, &uniform_names()) {
            Ok(p) => Slot::Ready(p),
            Err(err) => {
                tracing::warn!(
                    style = id,
                    ?err,
                    "compiling a transition shader; using the built-in style"
                );
                Slot::Failed
            }
        };
        self.programs.insert(id.to_owned(), slot);
        self.compiled = true;
    }

    fn program(&self, id: &str) -> Option<&GlesTexProgram> {
        match self.programs.get(id) {
            Some(Slot::Ready(p)) if self.usable(id) => Some(p),
            _ => None,
        }
    }

    /// The frame was drawn; `over` says whether it missed its budget. A style
    /// drawn in a run of [`OVERRUN_LIMIT`] missed frames is disabled for the
    /// session; a style drawn in a good frame starts counting again.
    pub fn frame_result(&mut self, over: bool) {
        let drawn = std::mem::take(&mut self.drawn);
        // The frame that compiled a program is slow for a reason that is not
        // the style's drawing.
        let over = over && !std::mem::take(&mut self.compiled);
        for id in drawn {
            if !over {
                self.overruns.remove(&*id);
                continue;
            }
            let n = self.overruns.entry(id.to_string()).or_insert(0);
            *n += 1;
            if *n >= OVERRUN_LIMIT && self.disabled.insert(id.to_string()) {
                tracing::warn!(
                    style = &*id,
                    "transition shader is over the frame budget; disabled for this session"
                );
            }
        }
    }

    /// Forget the offscreen copies of runs that are gone.
    pub fn retain_textures(&mut self, live: &HashSet<u64>) {
        self.textures.retain(|serial, _| live.contains(serial));
        self.trackers.retain(|serial, _| live.contains(serial));
        self.rendered.retain(|serial| live.contains(serial));
    }

    /// Whether the run's copy must be drawn into this frame. A close or
    /// minimize shows a frozen window, so it is drawn once; an open follows
    /// the live window. A fresh texture is empty either way.
    fn needs_render(&mut self, run: &ShaderRun, fresh: bool) -> bool {
        if fresh {
            self.rendered.remove(&run.serial);
            self.trackers.remove(&run.serial);
        }
        run.kind == RunKind::Open || !self.rendered.contains(&run.serial)
    }

    fn texture(
        &mut self,
        renderer: &mut GlesRenderer,
        serial: u64,
        size: Size<i32, Physical>,
    ) -> Result<(GlesTexture, bool), GlesError> {
        let want: Size<i32, BufferCoords> = size.to_logical(1).to_buffer(1, BufferTransform::Normal);
        if let Some(t) = self.textures.get(&serial) {
            use smithay::backend::renderer::Texture;
            if t.size() == want {
                return Ok((t.clone(), false));
            }
        }
        let t = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, want)?;
        self.textures.insert(serial, t.clone());
        Ok((t, true))
    }

    /// Draw the window (`content` builds its elements, with the geometry's
    /// top-left at `quad.origin()`) into the run's offscreen copy and return the
    /// element that draws it through the style, placed at `quad_loc`
    /// (output-local physical, the quad's top-left). `None`, with the
    /// reason logged, means the caller draws the built-in style instead.
    #[allow(clippy::too_many_arguments)]
    pub fn element<E: RenderElement<GlesRenderer>>(
        &mut self,
        renderer: &mut GlesRenderer,
        context: ContextId<GlesTexture>,
        run: &ShaderRun,
        now: Instant,
        win: Size<i32, Physical>,
        quad: Quad,
        quad_loc: Point<i32, Physical>,
        scale: Scale<f64>,
        content: impl FnOnce(&mut GlesRenderer) -> Vec<E>,
    ) -> Option<ShaderElement> {
        let program = self.program(&run.style)?.clone();
        let size = quad.size(win);
        if size.w <= 0 || size.h <= 0 || size.w > MAX_TEXTURE || size.h > MAX_TEXTURE {
            return None;
        }
        let (mut texture, fresh) = match self.texture(renderer, run.serial, size) {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(?err, "allocating a transition shader's offscreen copy");
                return None;
            }
        };
        if self.needs_render(run, fresh) {
            let els = content(renderer);
            // An open keeps its tracker: the copy persists between frames
            // (age 1), so an unchanged window costs nothing. A frozen ghost is
            // drawn once, whole.
            let keep = run.kind == RunKind::Open;
            let (mut tracker, age) = match self.trackers.remove(&run.serial) {
                Some(t) if keep => (t, 1),
                _ => (OutputDamageTracker::new(size, scale, BufferTransform::Normal), 0),
            };
            let rendered = (|| {
                let mut fb = Bind::bind(renderer, &mut texture).map_err(|_| ())?;
                tracker
                    .render_output(renderer, &mut fb, age, &els, Color32F::new(0.0, 0.0, 0.0, 0.0))
                    .map(|_| ())
                    .map_err(|_| ())
            })();
            if rendered.is_err() {
                tracing::warn!(
                    style = &*run.style,
                    "drawing a window into its transition shader's copy failed"
                );
                self.rendered.remove(&run.serial);
                return None;
            }
            if keep {
                self.trackers.insert(run.serial, tracker);
            }
            self.rendered.insert(run.serial);
        }
        // The first drawn frame starts the clock and is not held against the
        // style's frame budget (it also paid for the copy).
        let first = run.arm(now);
        if !first && !self.drawn.iter().any(|s| **s == *run.style) {
            self.drawn.push(run.style.clone());
        }
        self.frame = self.frame.wrapping_add(1);
        Some(ShaderElement::new(
            run.id.clone(),
            context,
            texture,
            Rectangle::new(quad_loc, size),
            CommitCounter::from(self.frame),
            program,
            run.uniforms(now, win, quad, scale),
        ))
    }
}

/// A window's offscreen copy drawn through a transition shader.
///
/// It reports no underlying storage, so no backend can mistake it for a client
/// buffer it could scan out, and a new commit every frame, so the whole quad is
/// damaged while the run plays: a shader may move any pixel.
#[derive(Debug)]
pub struct ShaderElement {
    inner: TextureRenderElement<GlesTexture>,
    region: Rectangle<i32, Physical>,
    commit: CommitCounter,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
}

impl ShaderElement {
    fn new(
        id: Id,
        context: ContextId<GlesTexture>,
        texture: GlesTexture,
        region: Rectangle<i32, Physical>,
        commit: CommitCounter,
        program: GlesTexProgram,
        uniforms: Vec<Uniform<'static>>,
    ) -> Self {
        let size = Size::<i32, Logical>::from((region.size.w, region.size.h));
        let inner = TextureRenderElement::from_static_texture(
            id,
            context,
            region.loc.to_f64(),
            texture,
            1,
            BufferTransform::Normal,
            None,
            None,
            Some(size),
            None,
            Kind::Unspecified,
        );
        Self {
            inner,
            region,
            commit,
            program,
            uniforms,
        }
    }
}

impl Element for ShaderElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn location(&self, _scale: Scale<f64>) -> Point<i32, Physical> {
        self.region.loc
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }

    fn transform(&self) -> BufferTransform {
        self.inner.transform()
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.region
    }

    fn damage_since(&self, _scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            DamageSet::default()
        } else {
            DamageSet::from_slice(&[Rectangle::from_size(self.region.size)])
        }
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        1.0
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for ShaderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        if damage.is_empty() {
            return Ok(());
        }
        frame.override_default_tex_program(self.program.clone(), self.uniforms.clone());
        let res = RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_abyss_config::animations::Curve;
    use ec_abyss_config::animations::{Override, Preset};

    fn style(id: &str, events: &[Event]) -> TransitionStyle {
        TransitionStyle {
            id: id.into(),
            label: id.into(),
            events: events.to_vec(),
            source: "void main() {}".into(),
            margin: 12,
            duration_ms: 400,
            curve: Curve::EaseOut,
            reach: false,
        }
    }

    fn anims(ev: Event, style: &str) -> Animations {
        let mut a = Animations {
            preset: Preset::Smooth,
            ..Animations::default()
        };
        a.overrides.insert(
            ev,
            Override {
                style: Some(style.into()),
                ..Default::default()
            },
        );
        a
    }

    fn start(reg: &mut Registry, a: &Animations, kind: RunKind, shed: ShedLevel) -> Option<ShaderRun> {
        reg.start(a, kind, Instant::now(), shed, (0.0, 0.0), 0.0)
    }

    #[test]
    fn an_installed_style_starts_with_its_own_duration() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::WindowClose])]);
        let a = anims(Event::WindowClose, "fx:a");
        let run = start(&mut reg, &a, RunKind::Close, ShedLevel::Full).expect("starts");
        assert_eq!(run.leg.duration.as_millis(), 400);
        assert_eq!(run.margin, 12);
        // The user's duration wins over the pack's.
        let mut a = a;
        a.overrides.get_mut(&Event::WindowClose).unwrap().duration_ms = Some(100);
        let run = start(&mut reg, &a, RunKind::Close, ShedLevel::Full).unwrap();
        assert_eq!(run.leg.duration.as_millis(), 100);
    }

    #[test]
    fn an_unknown_or_unserved_style_falls_back_without_an_error() {
        let mut reg = Registry::default();
        let a = anims(Event::WindowOpen, "gone:style");
        assert!(start(&mut reg, &a, RunKind::Open, ShedLevel::Full).is_none());
        assert!(reg.warned.contains("gone:style"), "warned once");
        reg.set_catalog(vec![style("fx:a", &[Event::WindowClose])]);
        let a = anims(Event::WindowOpen, "fx:a");
        assert!(start(&mut reg, &a, RunKind::Open, ShedLevel::Full).is_none());
    }

    #[test]
    fn a_built_in_style_never_starts_a_run() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::WindowOpen])]);
        assert!(start(&mut reg, &Animations::default(), RunKind::Open, ShedLevel::Full).is_none());
    }

    #[test]
    fn shedding_to_builtin_only_stops_shaders() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::Minimize])]);
        let a = anims(Event::Minimize, "fx:a");
        assert!(start(&mut reg, &a, RunKind::Minimize, ShedLevel::Full).is_some());
        for level in [ShedLevel::BuiltinOnly, ShedLevel::FadeOnly, ShedLevel::Off] {
            assert!(
                start(&mut reg, &a, RunKind::Minimize, level).is_none(),
                "{level:?}"
            );
        }
    }

    #[test]
    fn off_and_reduced_motion_start_nothing() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::WindowOpen])]);
        let mut a = anims(Event::WindowOpen, "fx:a");
        a.reduce_motion = true;
        assert!(start(&mut reg, &a, RunKind::Open, ShedLevel::Full).is_none());
        let mut a = anims(Event::WindowOpen, "fx:a");
        a.preset = Preset::Off;
        a.overrides.get_mut(&Event::WindowOpen).unwrap().style = Some("none".into());
        assert!(start(&mut reg, &a, RunKind::Open, ShedLevel::Full).is_none());
    }

    #[test]
    fn repeated_overruns_disable_a_style_for_the_session() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::WindowClose])]);
        let a = anims(Event::WindowClose, "fx:a");
        let id: Arc<str> = Arc::from("fx:a");
        for i in 0..OVERRUN_LIMIT {
            assert!(
                start(&mut reg, &a, RunKind::Close, ShedLevel::Full).is_some(),
                "{i}"
            );
            reg.drawn.push(id.clone());
            reg.frame_result(true);
        }
        assert!(reg.is_disabled("fx:a"));
        assert!(start(&mut reg, &a, RunKind::Close, ShedLevel::Full).is_none());
    }

    #[test]
    fn a_good_frame_resets_the_overrun_count() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::WindowClose])]);
        let id: Arc<str> = Arc::from("fx:a");
        for _ in 0..OVERRUN_LIMIT - 1 {
            reg.drawn.push(id.clone());
            reg.frame_result(true);
        }
        reg.drawn.push(id.clone());
        reg.frame_result(false);
        reg.drawn.push(id);
        reg.frame_result(true);
        assert!(!reg.is_disabled("fx:a"));
    }

    #[test]
    fn a_style_is_not_ready_until_compiled_and_a_failure_is_final() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::WindowClose])]);
        assert!(!reg.ready("fx:a"), "lazy: nothing compiled yet");
        reg.programs.insert("fx:a".into(), Slot::Failed);
        let a = anims(Event::WindowClose, "fx:a");
        assert!(!reg.ready("fx:a"));
        assert!(
            start(&mut reg, &a, RunKind::Close, ShedLevel::Full).is_none(),
            "failed styles do not restart"
        );
    }

    #[test]
    fn removing_the_catalog_stops_every_style() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::WindowClose])]);
        reg.programs.insert("fx:a".into(), Slot::Failed);
        reg.set_catalog(Vec::new());
        assert!(reg.programs.is_empty() && reg.styles().is_empty());
        let a = anims(Event::WindowClose, "fx:a");
        assert!(start(&mut reg, &a, RunKind::Close, ShedLevel::Full).is_none());
    }

    #[test]
    fn the_header_declares_every_uniform_the_run_writes() {
        for name in uniform_names() {
            assert!(HEADER.contains(&format!(" {};", name.name)), "{}", name.name);
        }
        assert!(HEADER.starts_with("#version 100"));
        assert!(HEADER.contains("//_DEFINES_"));
    }

    #[test]
    fn uniforms_follow_the_run() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::WindowClose])]);
        let a = anims(Event::WindowClose, "fx:a");
        let t0 = Instant::now();
        let run = reg
            .start(&a, RunKind::Close, t0, ShedLevel::Full, (10.0, 20.0), 0.0)
            .unwrap();
        assert_eq!(
            run.uniforms(
                t0,
                (100, 50).into(),
                run.quad((100, 50).into(), Scale::from(1.0)),
                Scale::from(1.0)
            )
            .len(),
            uniform_names().len()
        );
        run.arm(t0);
        assert_eq!(run.progress(t0), 0.0);
        let end = t0 + std::time::Duration::from_millis(500);
        assert_eq!(run.progress(end), 1.0);
        assert!(run.done(end) && !run.done(t0));
        // Closing: fully visible at the start, gone at the end.
        assert!((run.visible(t0) - 1.0).abs() < 1e-6 && run.visible(end).abs() < 1e-6);
        assert_eq!(margin_px(12, Scale::from(2.0)), 24);
    }

    fn run_at(reg: &mut Registry, kind: RunKind, t0: Instant) -> ShaderRun {
        let ev = kind.event();
        reg.set_catalog(vec![style("fx:a", &[ev])]);
        reg.start(&anims(ev, "fx:a"), kind, t0, ShedLevel::Full, (0.0, 0.0), 0.0)
            .unwrap()
    }

    #[test]
    fn an_armed_run_reads_zero_on_its_first_drawn_frame_whatever_the_wall_time() {
        let mut reg = Registry::default();
        let t0 = Instant::now();
        let run = run_at(&mut reg, RunKind::Close, t0);
        // Created, never drawn, a long time passes: still the start.
        let late = t0 + Duration::from_millis(300);
        assert_eq!(run.progress(late), 0.0);
        assert_eq!(run.eased(late), 0.0);
        assert!((run.visible(late) - 1.0).abs() < 1e-6);
        assert_eq!(run.time(late), 0.0);
        assert!(!run.done(late));
        // The first drawn frame stamps it, on a clone too.
        assert!(run.clone().arm(late));
        assert!(!run.arm(late + Duration::from_millis(5)), "first stamp wins");
        assert_eq!(run.progress(late), 0.0);
        assert_eq!(run.time(late), 0.0);
        let mid = late + Duration::from_millis(200);
        assert!((run.progress(mid) - 0.5).abs() < 1e-6);
        assert!(!run.done(mid) && run.done(late + Duration::from_millis(400)));
    }

    #[test]
    fn a_run_that_never_draws_still_ends() {
        let mut reg = Registry::default();
        let t0 = Instant::now();
        let run = run_at(&mut reg, RunKind::Close, t0);
        assert!(!run.done(t0 + Duration::from_millis(400)));
        assert!(run.done(t0 + Duration::from_millis(400) + UNARMED_GRACE));
    }

    #[test]
    fn a_ghost_copy_is_drawn_once_and_an_open_every_frame() {
        let mut reg = Registry::default();
        let t0 = Instant::now();
        let close = run_at(&mut reg, RunKind::Close, t0);
        assert!(reg.needs_render(&close, true));
        reg.rendered.insert(close.serial);
        for _ in 0..5 {
            assert!(!reg.needs_render(&close, false), "frozen: reused");
        }
        assert!(reg.needs_render(&close, true), "a new texture is empty");
        let min = run_at(&mut reg, RunKind::Minimize, t0);
        reg.rendered.insert(min.serial);
        assert!(!reg.needs_render(&min, false));
        let open = run_at(&mut reg, RunKind::Open, t0);
        reg.rendered.insert(open.serial);
        assert!(reg.needs_render(&open, false), "an open follows the live window");
        reg.retain_textures(&HashSet::new());
        assert!(reg.rendered.is_empty());
    }

    #[test]
    fn the_reach_quad_contains_the_target_in_every_direction() {
        let win = Size::<i32, Logical>::from((400, 300));
        for scale in [1.0, 1.5, 2.0] {
            let scale = Scale::from(scale);
            let wp: Size<i32, Physical> = win.to_f64().to_physical(scale).to_i32_round();
            for travel in [
                (900.0, 40.0),
                (-900.0, 40.0),
                (30.0, 1100.0),
                (-30.0, -1100.0),
                (700.0, -800.0),
                (0.0, 0.0),
                (-50.0, 20.0),
            ] {
                let run = ShaderRun {
                    reach: true,
                    travel,
                    margin: 16,
                    ..bare_run()
                };
                let q = run.quad(win, scale);
                let size = q.size(wp);
                // The target in the quad's own px.
                let tx = q.left as f64 + wp.w as f64 / 2.0 + travel.0 * scale.x;
                let ty = q.top as f64 + wp.h as f64 / 2.0 + travel.1 * scale.y;
                assert!(
                    tx > 0.0 && tx < size.w as f64 && ty > 0.0 && ty < size.h as f64,
                    "{travel:?} @{scale:?}: ({tx}, {ty}) in {size:?}"
                );
                // At least the plain margin on every side.
                let m = margin_px(16, scale);
                assert!(q.left >= m && q.top >= m && q.right >= m && q.bottom >= m);
            }
        }
    }

    #[test]
    fn without_reach_the_quad_is_the_margin_all_round() {
        let run = ShaderRun {
            travel: (900.0, 900.0),
            margin: 12,
            ..bare_run()
        };
        let q = run.quad((100, 80).into(), Scale::from(1.0));
        assert_eq!((q.left, q.top, q.right, q.bottom), (12, 12, 12, 12));
        let uni = run.uniforms(Instant::now(), (100, 80).into(), q, Scale::from(1.0));
        assert_eq!(uni.len(), uniform_names().len());
    }

    #[test]
    fn wanted_styles_come_from_the_config_and_the_presets() {
        let mut a = anims(Event::WindowClose, "fx:a");
        a.overrides.insert(
            Event::Minimize,
            Override {
                style: Some("scale".into()),
                ..Default::default()
            },
        );
        let w = wanted_styles(&a, &[]);
        assert_eq!(w, HashSet::from(["fx:a".to_string()]), "built-ins are not wanted");
        let preset = PackPreset {
            id: "fx:p".into(),
            label: "P".into(),
            base: Preset::Smooth,
            styles: vec![(Event::WindowOpen, "fx:b".into())],
        };
        let w = wanted_styles(&a, std::slice::from_ref(&preset));
        assert!(w.contains("fx:a") && w.contains("fx:b") && w.len() == 2);
        // The registry follows the config and the presets.
        let mut reg = Registry::default();
        reg.select(&a);
        assert!(reg.wanted().contains("fx:a") && !reg.wanted().contains("fx:b"));
        reg.set_presets(vec![preset]);
        reg.select(&a);
        assert!(reg.wanted().contains("fx:b"));
        reg.select(&Animations::default());
        assert!(!reg.wanted().contains("fx:a"));
    }

    #[test]
    fn the_frame_that_compiled_is_not_an_overrun() {
        let mut reg = Registry::default();
        reg.set_catalog(vec![style("fx:a", &[Event::WindowClose])]);
        reg.compiled = true;
        reg.drawn.push(Arc::from("fx:a"));
        reg.frame_result(true);
        assert!(!reg.overruns.contains_key("fx:a") && !reg.compiled);
        reg.drawn.push(Arc::from("fx:a"));
        reg.frame_result(true);
        assert_eq!(reg.overruns.get("fx:a"), Some(&1));
    }

    fn bare_run() -> ShaderRun {
        ShaderRun {
            style: Arc::from("fx:a"),
            kind: RunKind::Minimize,
            leg: Leg::new(Instant::now(), 400, Curve::EaseOut),
            margin: 0,
            reach: false,
            armed: Arc::new(OnceLock::new()),
            travel: (0.0, 0.0),
            side: 0.0,
            seed: 0.0,
            serial: 1,
            id: Id::new(),
        }
    }
}
