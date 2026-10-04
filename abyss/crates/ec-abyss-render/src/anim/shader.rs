// SPDX-License-Identifier: AGPL-3.0-only
//! Add-on transition shaders (ADR 0071 "Transition shaders"; ADR 0066 hook
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
use std::sync::Arc;
use std::time::Instant;

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
use ec_abyss_config::transitions::TransitionStyle;

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
/// - `time`: seconds since the run started.
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
    pub leg: Leg,
    /// Logical px the quad is grown by.
    pub margin: u32,
    /// Where the window is headed, from its centre, logical px.
    pub travel: (f64, f64),
    pub side: f32,
    pub seed: f32,
    /// Unique per run: keys its offscreen texture.
    pub serial: u64,
    /// The element's identity across frames.
    pub id: Id,
}

impl ShaderRun {
    /// Linear time through the run, 0 to 1.
    pub fn progress(&self, now: Instant) -> f32 {
        let total = self.leg.duration.as_secs_f64();
        if total <= 0.0 {
            return 1.0;
        }
        (now.saturating_duration_since(self.leg.start).as_secs_f64() / total).clamp(0.0, 1.0) as f32
    }

    /// The curve applied to [`ShaderRun::progress`]; may overshoot 1.0.
    pub fn eased(&self, now: Instant) -> f32 {
        (1.0 - self.leg.response(now).pd) as f32
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

    pub fn done(&self, now: Instant) -> bool {
        self.leg.done(now)
    }

    /// The uniform values for one frame. `win` is the window's physical size
    /// and `inset` the margin in physical px; `scale` converts `travel`.
    pub fn uniforms(
        &self,
        now: Instant,
        win: Size<i32, Physical>,
        inset: i32,
        scale: Scale<f64>,
    ) -> Vec<Uniform<'static>> {
        let quad = (win.w + 2 * inset, win.h + 2 * inset);
        let (qw, qh) = (quad.0.max(1) as f32, quad.1.max(1) as f32);
        let content = [
            inset as f32 / qw,
            inset as f32 / qh,
            win.w as f32 / qw,
            win.h as f32 / qh,
        ];
        let travel = [(self.travel.0 * scale.x) as f32, (self.travel.1 * scale.y) as f32];
        let elapsed = now.saturating_duration_since(self.leg.start).as_secs_f32();
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
    programs: HashMap<String, Slot>,
    /// Styles already warned about, so a missing pack logs once.
    warned: HashSet<String>,
    /// Off for the session: too slow.
    disabled: HashSet<String>,
    overruns: HashMap<String, u32>,
    /// Styles drawn since the last [`Registry::frame_result`].
    drawn: Vec<Arc<str>>,
    textures: HashMap<u64, GlesTexture>,
    serial: u64,
    frame: usize,
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
        let margin = style.margin;
        self.serial += 1;
        let serial = self.serial;
        Some(ShaderRun {
            style: Arc::from(r.style.as_str()),
            kind,
            leg: Leg::new(now, ms, curve),
            margin,
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
    }

    fn texture(
        &mut self,
        renderer: &mut GlesRenderer,
        serial: u64,
        size: Size<i32, Physical>,
    ) -> Result<GlesTexture, GlesError> {
        let want: Size<i32, BufferCoords> = size.to_logical(1).to_buffer(1, BufferTransform::Normal);
        if let Some(t) = self.textures.get(&serial) {
            use smithay::backend::renderer::Texture;
            if t.size() == want {
                return Ok(t.clone());
            }
        }
        let t = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, want)?;
        self.textures.insert(serial, t.clone());
        Ok(t)
    }

    /// Draw `content` (the window alone, laid out with its geometry's top-left
    /// at `inset` physical px) into the run's offscreen copy and return the
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
        quad_loc: Point<i32, Physical>,
        scale: Scale<f64>,
        content: &[E],
    ) -> Option<ShaderElement> {
        let program = self.program(&run.style)?.clone();
        let inset = margin_px(run.margin, scale);
        let size: Size<i32, Physical> = (win.w + 2 * inset, win.h + 2 * inset).into();
        if size.w <= 0 || size.h <= 0 || size.w > MAX_TEXTURE || size.h > MAX_TEXTURE {
            return None;
        }
        let mut texture = match self.texture(renderer, run.serial, size) {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(?err, "allocating a transition shader's offscreen copy");
                return None;
            }
        };
        let rendered = (|| {
            // Whole each frame (`age = 0`): the copy changes with the window
            // and the run, and nothing else shares this texture.
            let mut tracker = OutputDamageTracker::new(size, scale, BufferTransform::Normal);
            let mut fb = Bind::bind(renderer, &mut texture).map_err(|_| ())?;
            tracker
                .render_output(renderer, &mut fb, 0, content, Color32F::new(0.0, 0.0, 0.0, 0.0))
                .map(|_| ())
                .map_err(|_| ())
        })();
        if rendered.is_err() {
            tracing::warn!(
                style = &*run.style,
                "drawing a window into its transition shader's copy failed"
            );
            return None;
        }
        if !self.drawn.iter().any(|s| **s == *run.style) {
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
            run.uniforms(now, win, inset, scale),
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
            run.uniforms(t0, (100, 50).into(), 12, Scale::from(1.0)).len(),
            uniform_names().len()
        );
        assert_eq!(run.progress(t0), 0.0);
        let end = t0 + std::time::Duration::from_millis(500);
        assert_eq!(run.progress(end), 1.0);
        assert!(run.done(end) && !run.done(t0));
        // Closing: fully visible at the start, gone at the end.
        assert!((run.visible(t0) - 1.0).abs() < 1e-6 && run.visible(end).abs() < 1e-6);
        assert_eq!(margin_px(12, Scale::from(2.0)), 24);
    }
}
