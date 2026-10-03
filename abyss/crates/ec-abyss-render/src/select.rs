// SPDX-License-Identifier: AGPL-3.0-only
//! Compositor-drawn region selector (COMP-18 §1.1, §1.3).
//!
//! An addon that wants a rectangle must not be the thing that asks for it.
//! Letting Oracle-Eyes draw its own rubber-band would mean an untrusted client
//! grabbing the seat and painting over the whole screen on a chord it chose,
//! which is exactly the authority COMP-18 §1.3 denies it. So the compositor
//! runs the interaction itself and hands back only the four numbers, the same
//! way the overscan calibration overlay owns the seat for its own keys.
//!
//! Drawn as a backend-prepended pass, so it is invisible to `capture.rs` by
//! construction rather than by policy (ADR 0040) — a selector that showed up
//! in the capture it is about to trigger would be read back as screen content.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ec_abyss_config::Config;
use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            texture::{TextureBuffer, TextureRenderElement},
            Kind,
        },
        gles::{element::PixelShaderElement, GlesPixelProgram, GlesRenderer, GlesTexture},
    },
    input::keyboard::Keysym,
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
};

use super::{
    annotation::upload,
    effects,
    hud::{self, PILL_MARGIN},
    hud_font::{self, Face},
    palette::HudPalette,
    text::{self, Rgba},
    AbyssRenderElement,
};

/// Shortest edge, in logical pixels, that counts as a deliberate drag. Below
/// this a release is a miss-click, and COMP-18 §1.3 gives an addon nothing on
/// a miss-click: a 0x0 rectangle is still a rectangle it could act on.
const MIN_EDGE: i32 = 8;

/// The band: a 2px accent ring just outside the selection, outer radius 10,
/// so it frames what will be sent without covering any of it.
const BAND_WIDTH: i32 = 2;
const BAND_RADIUS: f32 = 8.0;

/// The size readout sits this far below and right of the cursor.
const READOUT_OFFSET: i32 = 12;
const READOUT_PAD: (usize, usize) = (6, 10);
/// The hint pill shown on entry, before the button goes down.
const HINT: &str = "Drag to select \u{b7} Esc to cancel";
const HINT_PAD: (usize, usize) = (6, 12);
const HINT_TOP: i32 = 24;

/// Motion: the dim fades in on entry, the hint fades out once the drag
/// starts.
const DIM_IN: Duration = Duration::from_millis(160);
const HINT_OUT: Duration = Duration::from_millis(220);

// What a key means while the selector owns the seat. Plain data carried by
// `input::Action`, so it lives in `ec-abyss-config`.
pub use ec_abyss_config::input::SelectKey;

/// Escape cancels, and so does a second press of the chord that started the
/// selection: the chord is a toggle, because a modal mode you can only leave
/// by a key you were not told about is a trap.
pub fn key(sym: Keysym, select_chord: bool) -> SelectKey {
    if select_chord || sym == Keysym::Escape {
        SelectKey::Cancel
    } else {
        SelectKey::Ignored
    }
}

/// The `keybind` payload for a committed region, in compositor-logical
/// coordinates — the same space `annotation_create` takes rectangles in, so
/// the addon can hand it straight back without knowing about output scale.
pub fn payload(rect: Rectangle<i32, Logical>) -> serde_json::Value {
    serde_json::json!({
        "action": "annotation-select",
        "region": {
            "x": rect.loc.x,
            "y": rect.loc.y,
            "w": rect.size.w,
            "h": rect.size.h,
        },
    })
}

/// A drag in progress, in global logical coordinates.
#[derive(Debug)]
struct Session {
    /// Where the button went down. `None` until it does: the mode is entered
    /// by a chord, which leaves the human free to move the pointer first.
    anchor: Option<Point<i32, Logical>>,
    cursor: Point<i32, Logical>,
    /// When the mode was entered and when the button went down, for the
    /// dim's fade-in and the hint's fade-out.
    started: Instant,
    pressed: Option<Instant>,
}

/// Selection-mode state. Inactive is the default and costs a frame nothing.
#[derive(Debug, Default)]
pub struct RegionSelect {
    session: Option<Session>,
    /// Per-output render state kept across frames, so an unchanged band or
    /// pill keeps its element id and commit and is not repainted. Dropped
    /// when the session ends.
    cache: HashMap<Output, Cache>,
    /// The band program, compiled on first use; shared by every output.
    border: Option<GlesPixelProgram>,
    border_failed: bool,
    /// `animations.enabled` as of the last frame drawn.
    motion: bool,
}

#[derive(Debug, Default)]
struct Cache {
    band: Option<PixelShaderElement>,
    band_color: [f32; 4],
    /// Above, below, left and right of the selection; the first alone is the
    /// whole output before the drag starts.
    dim: [SolidColorBuffer; 4],
    /// Keyed by (w, h, raster scale).
    readout: Option<((i32, i32, usize), TextureBuffer<GlesTexture>)>,
    hint: Option<(usize, TextureBuffer<GlesTexture>)>,
}

impl RegionSelect {
    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    /// Enter selection mode with the pointer where it already is.
    pub fn start(&mut self, cursor: Point<i32, Logical>) {
        self.session = Some(Session {
            anchor: None,
            cursor,
            started: Instant::now(),
            pressed: None,
        });
    }

    /// Leave selection mode with nothing to report.
    pub fn cancel(&mut self) {
        self.session = None;
        self.cache.clear();
    }

    /// Returns whether the drawn shape changed, so a motion that only moves
    /// the (compositor-drawn) cursor does not cost a full redraw.
    pub fn motion(&mut self, pos: Point<i32, Logical>) -> bool {
        let Some(s) = self.session.as_mut() else {
            return false;
        };
        let moved = s.cursor != pos;
        s.cursor = pos;
        moved && s.anchor.is_some()
    }

    pub fn press(&mut self, pos: Point<i32, Logical>) {
        if let Some(s) = self.session.as_mut() {
            s.anchor = Some(pos);
            s.cursor = pos;
            s.pressed = Some(Instant::now());
        }
    }

    /// Ends the session either way. `Some` only for a deliberate drag: a
    /// press-release with no travel is a cancel, not a 0x0 rectangle.
    pub fn release(&mut self, pos: Point<i32, Logical>) -> Option<Rectangle<i32, Logical>> {
        let s = self.session.take()?;
        self.cache.clear();
        let anchor = s.anchor?;
        let rect = normalize(anchor, pos);
        (rect.size.w >= MIN_EDGE && rect.size.h >= MIN_EDGE).then_some(rect)
    }

    /// The rubber-band as it currently stands, if the drag has started.
    pub fn rect(&self) -> Option<Rectangle<i32, Logical>> {
        let s = self.session.as_ref()?;
        Some(normalize(s.anchor?, s.cursor))
    }

    /// Whether a fade is in flight, so the next frame must be drawn even
    /// with no damage. False whenever animations are off.
    pub fn animating(&self) -> bool {
        let Some(s) = self.session.as_ref().filter(|_| self.motion) else {
            return false;
        };
        let now = Instant::now();
        now.duration_since(s.started) < DIM_IN || s.pressed.is_some_and(|p| now.duration_since(p) < HINT_OUT)
    }
}

/// Two drag points in any order into a rectangle with non-negative extents.
pub fn normalize(a: Point<i32, Logical>, b: Point<i32, Logical>) -> Rectangle<i32, Logical> {
    let loc: Point<i32, Logical> = (a.x.min(b.x), a.y.min(b.y)).into();
    let size: Size<i32, Logical> = ((a.x - b.x).abs(), (a.y - b.y).abs()).into();
    Rectangle::new(loc, size)
}

fn phys(p: Point<i32, Logical>, scale: Scale<f64>) -> Point<i32, Physical> {
    p.to_f64().to_physical(scale).to_i32_round()
}

/// Straight RGBA, faded by `k`, premultiplied: what `SolidColorBuffer` and the
/// border shader take.
fn premul(c: Rgba, k: f32) -> [f32; 4] {
    let a = c[3] * k.clamp(0.0, 1.0);
    [c[0] * a, c[1] * a, c[2] * a, a]
}

fn eased(now: Instant, since: Instant, over: Duration) -> f32 {
    let t = (now.duration_since(since).as_secs_f32() / over.as_secs_f32()).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// The dim's four rectangles around `r` (or the whole output, then three
/// empty ones, when there is no selection on this output yet).
fn dim_rects(
    r: Option<Rectangle<i32, Logical>>,
    logical: Size<i32, Logical>,
) -> [Rectangle<i32, Logical>; 4] {
    let rect = |x, y, w, h| Rectangle::new(Point::from((x, y)), Size::from((w, h)));
    let Some(r) = r else {
        let none = rect(0, 0, 0, 0);
        return [rect(0, 0, logical.w, logical.h), none, none, none];
    };
    let (right, bottom) = (r.loc.x + r.size.w, r.loc.y + r.size.h);
    [
        rect(0, 0, logical.w, r.loc.y),
        rect(0, bottom, logical.w, logical.h - bottom),
        rect(0, r.loc.y, r.loc.x, r.size.h),
        rect(right, r.loc.y, logical.w - right, r.size.h),
    ]
}

/// The readout's text: the selection's size, `412 × 128`.
fn readout_text(w: i32, h: i32) -> String {
    format!("{w} \u{d7} {h}")
}

/// Where the readout pill goes: below-right of the cursor, kept on the
/// output.
fn readout_at(
    cursor: Point<i32, Logical>,
    size: (usize, usize),
    logical: Size<i32, Logical>,
) -> Point<i32, Logical> {
    let (w, h) = (size.0 as i32, size.1 as i32);
    let mut x = cursor.x + READOUT_OFFSET;
    let mut y = cursor.y + READOUT_OFFSET;
    if x + w > logical.w {
        x = cursor.x - READOUT_OFFSET - w;
    }
    if y + h > logical.h {
        y = cursor.y - READOUT_OFFSET - h;
    }
    Point::from((x.max(0), y.max(0)))
}

fn readout_pill(w: i32, h: i32, pal: &HudPalette, dev: usize) -> (text::Raster, (usize, usize)) {
    hud::pill(
        &readout_text(w, h),
        Face::Readout,
        READOUT_PAD,
        pal.fallback_fill,
        Some(pal.panel_rim),
        pal.keyline,
        pal.text,
        dev,
    )
}

fn hint_pill(pal: &HudPalette, dev: usize) -> (text::Raster, (usize, usize)) {
    hud::pill(
        HINT,
        Face::Body,
        HINT_PAD,
        pal.fallback_fill,
        Some(pal.panel_rim),
        pal.keyline,
        pal.text_secondary,
        dev,
    )
}

fn hint_at(size: (usize, usize), logical: Size<i32, Logical>) -> Point<i32, Logical> {
    Point::from((((logical.w - size.0 as i32) / 2).max(0), HINT_TOP))
}

/// The selector's elements for one output, front-to-back. Empty when no
/// selection is running, which is the overwhelmingly common case.
///
/// Front to back: the readout and hint pills, the band, the dim.
pub fn selector_elements(
    renderer: &mut GlesRenderer,
    select: &mut RegionSelect,
    output: &Output,
    output_loc: Point<i32, Logical>,
    config: &Config,
) -> Vec<AbyssRenderElement> {
    select.motion = config.animations.enabled;
    let Some(session) = select.session.as_ref() else {
        return Vec::new();
    };
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let now = Instant::now();
    let (started, pressed, cursor) = (session.started, session.pressed, session.cursor - output_loc);
    let fractional = output.current_scale().fractional_scale();
    let scale = Scale::from(fractional);
    let dev = hud_font::raster_scale(fractional);
    let logical: Size<i32, Logical> = mode.size.to_f64().to_logical(scale).to_i32_round();
    let bounds = Rectangle::new(Point::from((0, 0)), logical);
    let pal = HudPalette::new(&config.annotations);
    let motion = select.motion;
    let fade_in = if motion { eased(now, started, DIM_IN) } else { 1.0 };
    let hint_alpha = match pressed {
        None => 1.0,
        Some(p) if motion => 1.0 - eased(now, p, HINT_OUT),
        Some(_) => 0.0,
    };

    // Output-local, because each output draws its own share of a rectangle
    // that lives in the global space.
    let whole = select.rect().map(|r| Rectangle::new(r.loc - output_loc, r.size));
    let local = whole.and_then(|r| r.intersection(bounds));

    if select.border.is_none() && !select.border_failed {
        match effects::compile_border(renderer) {
            Ok(p) => select.border = Some(p),
            Err(err) => {
                tracing::warn!(?err, "compiling the selector band; drawing the dim only");
                select.border_failed = true;
            }
        }
    }
    let border = select.border.clone();
    let cache = select.cache.entry(output.clone()).or_default();
    let mut out = Vec::new();
    let texture = |buffer: &TextureBuffer<GlesTexture>, at: Point<i32, Logical>, alpha: f32| {
        AbyssRenderElement::Texture(TextureRenderElement::from_texture_buffer(
            at.to_f64().to_physical(scale),
            buffer,
            (alpha < 1.0).then_some(alpha.max(0.0)),
            None,
            None,
            Kind::Unspecified,
        ))
    };

    // The readout, while there is a drag to measure.
    if let Some(r) = whole.filter(|_| local.is_some()) {
        let key = (r.size.w, r.size.h, dev);
        if cache.readout.as_ref().map(|(k, _)| *k != key).unwrap_or(true) {
            let (raster, _) = readout_pill(r.size.w, r.size.h, &pal, dev);
            cache.readout = upload(renderer, &raster, dev).map(|b| (key, b));
        }
        if let Some((_, buffer)) = &cache.readout {
            let size = hud::pill_size(&readout_text(r.size.w, r.size.h), Face::Readout, READOUT_PAD);
            let m = PILL_MARGIN as i32;
            let at = readout_at(cursor, size, logical) - Point::from((m, m));
            out.push(texture(buffer, at, fade_in));
        }
    }

    // The hint, until it has faded after the press.
    if hint_alpha > 0.0 {
        if cache.hint.as_ref().map(|(d, _)| *d != dev).unwrap_or(true) {
            let (raster, _) = hint_pill(&pal, dev);
            cache.hint = upload(renderer, &raster, dev).map(|b| (dev, b));
        }
        if let Some((_, buffer)) = &cache.hint {
            let size = hud::pill_size(HINT, Face::Body, HINT_PAD);
            let m = PILL_MARGIN as i32;
            out.push(texture(
                buffer,
                hint_at(size, logical) - Point::from((m, m)),
                hint_alpha * fade_in,
            ));
        }
    }

    // The band, one rounded ring just outside the selection.
    if let (Some(r), Some(program)) = (local, border) {
        let g = BAND_WIDTH;
        let area = Rectangle::new(r.loc - Point::from((g, g)), r.size + Size::from((2 * g, 2 * g)));
        let color = premul(pal.accent, fade_in);
        let uniforms = || effects::border_uniforms(color, BAND_RADIUS, g as f32, fractional as f32);
        let band = cache.band.get_or_insert_with(|| {
            PixelShaderElement::new(program, area, None, 1.0, uniforms(), Kind::Unspecified)
        });
        band.resize(area, None);
        if cache.band_color != color {
            band.update_uniforms(uniforms());
            cache.band_color = color;
        }
        out.push(AbyssRenderElement::Shader(band.clone()));
    }

    // The dim: what will not be sent.
    let dim = premul(pal.selection_dim, fade_in);
    for (buffer, r) in cache.dim.iter_mut().zip(dim_rects(local, logical)) {
        if r.size.w <= 0 || r.size.h <= 0 {
            continue;
        }
        buffer.update(r.size, dim);
        out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
            buffer,
            phys(r.loc, scale),
            scale,
            1.0,
            Kind::Unspecified,
        )));
    }
    out
}

/// The selector as the dump harness draws it: every raster, front to back,
/// output-local and logical. The band is drawn with `hud::shape` here, the
/// GPU draws it with the border shader; same geometry, same colours.
#[cfg(test)]
pub(crate) fn sketch(
    rect: Option<Rectangle<i32, Logical>>,
    cursor: Point<i32, Logical>,
    logical: Size<i32, Logical>,
    pal: &HudPalette,
    dev: usize,
) -> Vec<(Point<i32, Logical>, text::Raster)> {
    let m = PILL_MARGIN as i32;
    let mut out = Vec::new();
    match rect {
        Some(r) => {
            let (raster, size) = readout_pill(r.size.w, r.size.h, pal, dev);
            out.push((readout_at(cursor, size, logical) - Point::from((m, m)), raster));
            let g = BAND_WIDTH;
            out.push((
                r.loc - Point::from((g, g)),
                hud::shape(
                    (r.size.w + 2 * g) as usize,
                    (r.size.h + 2 * g) as usize,
                    BAND_RADIUS + g as f32,
                    None,
                    Some((g as f32, pal.accent)),
                    dev,
                ),
            ));
        }
        None => {
            let (raster, size) = hint_pill(pal, dev);
            out.push((hint_at(size, logical) - Point::from((m, m)), raster));
        }
    }
    for r in dim_rects(rect, logical) {
        if r.size.w > 0 && r.size.h > 0 {
            out.push((
                r.loc,
                hud::shape(
                    r.size.w as usize,
                    r.size.h as usize,
                    0.0,
                    Some(pal.selection_dim),
                    None,
                    dev,
                ),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i32, y: i32) -> Point<i32, Logical> {
        (x, y).into()
    }

    #[test]
    fn a_drag_normalises_the_same_way_in_every_direction() {
        let want = Rectangle::new(p(10, 20), Size::from((30, 40)));
        for (a, b) in [
            (p(10, 20), p(40, 60)),
            (p(40, 60), p(10, 20)),
            (p(40, 20), p(10, 60)),
            (p(10, 60), p(40, 20)),
        ] {
            assert_eq!(normalize(a, b), want, "{a:?} -> {b:?}");
        }
    }

    #[test]
    fn a_drag_that_never_moved_is_a_cancel_not_a_zero_rect() {
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        s.press(p(100, 100));
        assert_eq!(s.release(p(100, 100)), None);
        assert!(!s.active(), "a release always ends the session");
    }

    #[test]
    fn a_sub_threshold_drag_is_a_cancel() {
        for end in [p(103, 140), p(140, 103), p(103, 103)] {
            let mut s = RegionSelect::default();
            s.start(p(0, 0));
            s.press(p(100, 100));
            assert_eq!(s.release(end), None, "{end:?}");
        }
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        s.press(p(100, 100));
        assert_eq!(
            s.release(p(100 + MIN_EDGE, 100 + MIN_EDGE)),
            Some(Rectangle::new(p(100, 100), Size::from((MIN_EDGE, MIN_EDGE))))
        );
    }

    #[test]
    fn a_release_before_any_press_commits_nothing() {
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        assert_eq!(s.release(p(500, 500)), None);
    }

    #[test]
    fn escape_and_the_chord_cancel_and_everything_else_is_swallowed() {
        assert_eq!(key(Keysym::Escape, false), SelectKey::Cancel);
        assert_eq!(key(Keysym::o, true), SelectKey::Cancel);
        assert_eq!(key(Keysym::a, false), SelectKey::Ignored);
        assert_eq!(key(Keysym::Return, false), SelectKey::Ignored);
    }

    #[test]
    fn cancelling_leaves_no_rectangle_to_emit() {
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        s.press(p(10, 10));
        s.motion(p(400, 400));
        assert!(s.rect().is_some(), "a live drag has a band to draw");
        s.cancel();
        assert!(!s.active());
        assert_eq!(s.rect(), None);
        // And the only thing that ever produces a payload is `release`.
        assert_eq!(s.release(p(400, 400)), None);
    }

    #[test]
    fn a_committed_rectangle_has_strictly_positive_extents() {
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        s.press(p(400, 400));
        let r = s.release(p(100, 100)).expect("a backwards drag still commits");
        assert!(r.size.w > 0 && r.size.h > 0);
        assert_eq!(r, Rectangle::new(p(100, 100), Size::from((300, 300))));
    }

    #[test]
    fn the_payload_carries_the_rectangle_in_logical_coordinates() {
        let v = payload(Rectangle::new(p(12, 34), Size::from((56, 78))));
        assert_eq!(
            v,
            serde_json::json!({
                "action": "annotation-select",
                "region": {"x": 12, "y": 34, "w": 56, "h": 78},
            })
        );
    }

    #[test]
    fn the_dim_leaves_exactly_the_selection_clear() {
        let out: Size<i32, Logical> = (1920, 1080).into();
        let r = Rectangle::new(p(100, 200), Size::from((300, 400)));
        let rects = dim_rects(Some(r), out);
        let area: i32 = rects.iter().map(|d| d.size.w * d.size.h).sum();
        assert_eq!(area, 1920 * 1080 - 300 * 400);
        assert!(rects.iter().all(|d| !d.overlaps(r)));
        assert_eq!(dim_rects(None, out)[0].size, out);
    }

    #[test]
    fn the_readout_reads_w_by_h_and_stays_on_the_output() {
        assert_eq!(readout_text(412, 128), "412 \u{d7} 128");
        let out: Size<i32, Logical> = (800, 600).into();
        let at = readout_at(p(100, 100), (90, 26), out);
        assert_eq!(at, p(112, 112));
        let at = readout_at(p(790, 590), (90, 26), out);
        assert!(at.x + 90 <= 800 && at.y + 26 <= 600, "{at:?}");
    }

    #[test]
    fn nothing_animates_with_animations_off_or_no_session() {
        let mut s = RegionSelect::default();
        assert!(!s.animating());
        s.motion = true;
        s.start(p(0, 0));
        assert!(s.animating(), "the dim is fading in");
        s.motion = false;
        assert!(!s.animating());
    }
}
