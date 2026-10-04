// SPDX-License-Identifier: AGPL-3.0-only
//! The annotation pass (COMP-18, ADR 0040, ADR 0054, ADR 0071).
//!
//! A compositor-drawn pass above everything client-drawn and above the cursor,
//! and **below** trusted UI. It is not trusted UI and carries no phrase: the
//! text in it is written by an untrusted caller -- for Oracle-Eyes, by a model
//! summarising pixels an attacker chose -- and the whole point of ADR 0040 is
//! that such text must never share a position with the anti-spoof anchor.
//!
//! Two properties come from where it is drawn rather than from any check here:
//!
//! * **Capture-invisible.** `capture::capture_elements` builds its own
//!   pass list from the space and the layer map. Backend-prepended elements are
//!   not in it, so an annotation cannot appear in a screenshot, a screencast, or
//!   in the pixels Oracle-Eyes itself reads back. The pick marker, the region
//!   rim, the leader and the panel's blurred backdrop are all part of the same
//!   pass and inherit this unchanged. The backdrop *samples* what is behind
//!   the panel, exactly as a window's blur does; it draws nothing a capture
//!   could see.
//! * **Below trusted UI.** The per-frame element vector is front-to-back, so the
//!   backends splice annotations in *after* the indicator, never before.
//!
//! The caller supplies an optional rectangle, a kind, a title, a string and
//! optionally a pick. Everything else -- clamping, sanitising, the palette
//! (the owner's `annotations { … }` config, never the caller's), the panel's
//! width, where it lands and how it moves -- is decided here.
//!
//! Look (ADR 0071): a rounded glass panel, the region it is about ringed by
//! a thin rounded rim, a leader only when the panel had to sit away from it,
//! and -- when the caller names a pick -- the only gold on screen: a solid tab
//! beside the option and a faint wash over it.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use ec_abyss_config::{Blur, BlurMode, Config};
use smithay::{
    backend::allocator::Fourcc,
    backend::renderer::{
        element::{
            texture::{TextureBuffer, TextureRenderElement},
            Kind,
        },
        gles::{GlesRenderer, GlesTexProgram, GlesTexture},
        ImportMem,
    },
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

use super::{
    blur::{BlurKey, BlurStore},
    effects,
    hud::{self, Card},
    hud_font::{self, Face, ADVANCE},
    palette::HudPalette,
    text, AbyssRenderElement,
};

/// Columns a panel wraps at: chosen per output from this range, never by the
/// caller, so a caller cannot pick a width that shoulders other content off
/// the screen. See [`columns`].
const MIN_COLS: usize = 28;
const MAX_COLS: usize = 48;
/// A panel stacked above or below its region is never narrower than this:
/// a tall thin panel under a wide paragraph reads as a sidebar that lost its
/// way.
const STACK_MIN_COLS: usize = 40;

/// Longest title kept, in characters, before wrapping. ADR 0054: a headline,
/// not a second body.
const MAX_TITLE: usize = 80;

/// Gap between the region (and the pick tab) and the panel beside it.
const GUTTER: i32 = 18;
/// A leader is drawn only across a gap at least this wide. A panel sitting
/// right beside its region needs no line to say which region it means.
const LEADER_MIN: i32 = 24;
/// Diameter of the dot that ends a leader, outside the region rim.
const LEADER_DOT: i32 = 4;
/// An unanchored panel's distance from the top of the output.
const TOP_MARGIN: i32 = 24;
/// Space between unanchored panels stacked down from the top.
const STACK_GAP: i32 = 12;

/// The pick wash grows the option by this much on every side, and is this
/// round.
const WASH_GROW: i32 = 2;
const WASH_RADIUS: f32 = 6.0;
/// Horizontal padding inside the pick tab, and the gap between it and the
/// wash.
const TAB_PAD_X: usize = 6;
const TAB_GAP: i32 = 4;

/// Motion (spec item 6). Entering: fade in and slide 6px away from the
/// region, cubic ease-out. Leaving: fade out, ease-in. An update cross-fades
/// the panel.
const ENTER: Duration = Duration::from_millis(220);
const EXIT: Duration = Duration::from_millis(160);
const CROSSFADE: Duration = Duration::from_millis(120);
const SLIDE: f64 = 6.0;

/// The blur behind a glass panel: a 7px kernel over two passes, the nearest
/// the dual-filter chain gets to the spec's 28px.
const GLASS_BLUR_SIZE: i32 = 7;
const GLASS_BLUR_PASSES: i32 = 2;

/// Opaque handle to a live annotation. Handed back over the control socket;
/// the caller can address only annotations it created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnnotationId(pub u64);

/// What an annotation is. Chosen by the caller from a closed set; it changes
/// how the panel looks, never what the compositor lets it do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    /// An answer about a region. May carry a pick.
    Answer,
    /// Something went wrong. A danger dot before the title, and no pick --
    /// an error has nothing to point at, so it gets no gold.
    Error,
}

/// The one option inside the anchor a caller claims is the answer (ADR 0054).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// The option's rectangle, logical and global like the anchor. Must lie
    /// inside the anchor or the pick is dropped at the door.
    pub rect: Rectangle<i32, Logical>,
    /// One or two ASCII letters or digits, upper-cased. See [`pick_label`].
    pub label: String,
}

/// One live annotation: the region it is about, and what to say about it.
#[derive(Debug, Clone)]
pub struct Annotation {
    /// Control-socket connection that created this annotation. A caller may
    /// address only its own handles, and everything it made goes away with it.
    pub owner: u64,
    /// Region of the output this annotation refers to, in logical coordinates.
    /// `None` for an unanchored note, drawn top-centre with nothing to point at.
    pub anchor: Option<Rectangle<i32, Logical>>,
    pub kind: AnnotationKind,
    /// Already-sanitised title and body. Stored reduced so a caller's string
    /// is dealt with once, at the door; wrapping waits for the output, whose
    /// width decides the column count.
    pub title: String,
    pub text: String,
    pub pick: Option<Pick>,
    /// When it was created, for the entrance.
    born: Instant,
    /// When its text last changed, for the cross-fade.
    changed: Option<Instant>,
}

/// Live annotations, owned by `AbyssState`.
///
/// A plain map keyed by handle, per the handle-based-state invariant -- no
/// `Rc<RefCell<_>>`, nothing shared with another thread. Insertion order is not
/// meaningful; `next` only ever counts up, so a destroyed handle is never
/// reused and a stale reference fails closed.
#[derive(Debug, Default)]
pub struct AnnotationStore {
    live: BTreeMap<AnnotationId, Annotation>,
    /// Destroyed annotations still fading out, with when they went. Not
    /// addressable: every socket operation sees only `live`. Pruned once the
    /// exit is over, and never more than [`MAX_LIVE`].
    leaving: BTreeMap<AnnotationId, (Annotation, Instant)>,
    next: u64,
    /// The uploaded artwork for each annotation, per output, kept across
    /// frames. `TextureBuffer::from_texture` mints a fresh element id and a
    /// zeroed commit counter, so rebuilding it every frame would leave the
    /// damage tracker unable to match an element to last frame's and force a
    /// repaint of that region forever. Rebuilt only when [`ArtKey`] changes.
    /// Per output so two outputs at different scales do not thrash it.
    art: HashMap<Output, BTreeMap<AnnotationId, Art>>,
    /// The panels' blurred backdrops, per output. Their own store, not the
    /// windows': an annotation is not a surface and must not share a damage
    /// history with one.
    blur: HashMap<Output, BlurStore>,
    /// The backdrop program, compiled on first use. `glass_failed` remembers
    /// a compile error so it is reported once and the fallback used after.
    glass: Option<GlesTexProgram>,
    glass_failed: bool,
    /// `animations.enabled` as of the last frame drawn, so [`Self::animating`]
    /// asks for frames only when there is motion to draw.
    motion: bool,
}

/// Everything that decides what an annotation looks like and where each piece
/// of it lands. Cheap to compute every frame; the artwork behind it is not.
#[derive(Debug, Clone, PartialEq)]
struct ArtKey {
    card: Card,
    dev: usize,
    palette: HudPalette,
    glass: bool,
    region: Option<Rectangle<i32, Logical>>,
    pick: Option<Rectangle<i32, Logical>>,
    panel: Rectangle<i32, Logical>,
}

/// One uploaded piece, output-local and logical. `panel` marks the panel
/// itself: it alone slides and cross-fades, and the glass goes behind it.
#[derive(Debug)]
struct Part {
    at: Point<i32, Logical>,
    buffer: TextureBuffer<GlesTexture>,
    panel: bool,
}

/// One annotation's uploaded pieces, front to back: the panel, then the pick
/// tab and wash, the leader and the rim. `prev` is the panel as it was
/// before an update, kept for the cross-fade.
#[derive(Debug)]
struct Art {
    key: ArtKey,
    parts: Vec<Part>,
    prev: Option<Part>,
}

/// Furthest a caller's rectangle may sit from the origin, and the largest it
/// may be, in logical pixels. Well past any real output, but small enough that
/// every `loc + size` below stays inside `i32`: the anchor is drawn, not just
/// used for placement, and an unprivileged socket caller must not be able
/// to abort the compositor with an overflow. Clipping bounds the pixels; this
/// bounds the arithmetic.
const MAX_COORD: i32 = 1 << 20;

/// Reduce a caller's rectangle at the door, the same way its text is.
fn clamp_anchor(r: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((
            r.loc.x.clamp(-MAX_COORD, MAX_COORD),
            r.loc.y.clamp(-MAX_COORD, MAX_COORD),
        )),
        Size::from((r.size.w.clamp(0, MAX_COORD), r.size.h.clamp(0, MAX_COORD))),
    )
}

/// A pick label as the compositor will draw it: one or two ASCII letters or
/// digits, upper-cased. Anything else is `None` -- the label is drawn in a
/// tab beside the caller's region and is the one piece of caller text that
/// is not in the panel, so it gets the narrowest alphabet there is.
pub fn pick_label(s: &str) -> Option<String> {
    let ok = (1..=2).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric());
    ok.then(|| s.to_ascii_uppercase())
}

/// Keep a pick only if it is well-formed and lies wholly inside the anchor
/// (ADR 0054). The marker is on-screen geometry, and the anchor is the one
/// region this caller was already allowed to mark; outside it, a pick would
/// let the caller highlight anything at all.
fn admit(anchor: Rectangle<i32, Logical>, pick: Option<Pick>) -> Option<Pick> {
    let p = pick?;
    let rect = clamp_anchor(p.rect);
    let label = pick_label(&p.label)?;
    (rect.size.w > 0 && rect.size.h > 0 && anchor.contains_rect(rect)).then_some(Pick { rect, label })
}

/// Hard cap on live annotations. A caller that leaks handles degrades its own
/// display, not the compositor's memory.
pub const MAX_LIVE: usize = 32;

impl AnnotationStore {
    /// Sanitise `title` and `text`, admit `pick`, and store it all against a
    /// fresh handle owned by `owner`. Returns `None` once [`MAX_LIVE`]
    /// annotations are live. A pick that fails [`admit`] is dropped and the
    /// annotation is kept: the text is still worth showing. A pick needs an
    /// anchor to lie inside and an answer to mark, so an unanchored or error
    /// annotation never keeps one -- an error draws no gold.
    pub fn create(
        &mut self,
        owner: u64,
        anchor: Option<Rectangle<i32, Logical>>,
        kind: AnnotationKind,
        title: &str,
        text: &str,
        pick: Option<Pick>,
    ) -> Option<AnnotationId> {
        if self.live.len() >= MAX_LIVE {
            return None;
        }
        self.next += 1;
        let id = AnnotationId(self.next);
        let anchor = anchor.map(clamp_anchor);
        let pick = match (anchor, kind) {
            (Some(anchor), AnnotationKind::Answer) => admit(anchor, pick),
            _ => None,
        };
        self.live.insert(
            id,
            Annotation {
                owner,
                anchor,
                kind,
                title: title_for(title),
                text: text::sanitize(text),
                pick,
                born: Instant::now(),
                changed: None,
            },
        );
        Some(id)
    }

    /// Replace the title and text on a live handle (the expand flow, COMP-18
    /// §3). The pick stays: moving it is a new annotation. Returns false for a
    /// handle that does not exist -- an unknown handle is not an error the
    /// caller gets to distinguish from a destroyed one.
    pub fn update(&mut self, owner: u64, id: AnnotationId, title: &str, text: &str) -> bool {
        match self.live.get_mut(&id).filter(|a| a.owner == owner) {
            Some(a) => {
                let (title, text) = (title_for(title), text::sanitize(text));
                if a.title != title || a.text != text {
                    a.title = title;
                    a.text = text;
                    a.changed = Some(Instant::now());
                }
                true
            }
            None => false,
        }
    }

    /// Drop one handle. A handle owned by someone else is indistinguishable
    /// from one that never existed.
    pub fn destroy(&mut self, owner: u64, id: AnnotationId) -> bool {
        match self.live.get(&id) {
            Some(a) if a.owner == owner => {
                if let Some(a) = self.live.remove(&id) {
                    self.leave(id, a);
                }
                true
            }
            _ => false,
        }
    }

    /// Drop everything `owner` created. Also the disconnect path: a caller
    /// that goes away leaves nothing on screen once the exit fade is over.
    pub fn clear_for(&mut self, owner: u64) -> usize {
        let gone: Vec<AnnotationId> = self
            .live
            .iter()
            .filter(|(_, a)| a.owner == owner)
            .map(|(id, _)| *id)
            .collect();
        for id in &gone {
            if let Some(a) = self.live.remove(id) {
                self.leave(*id, a);
            }
        }
        gone.len()
    }

    /// Start an annotation's exit. Only for drawing: it is out of `live`, so
    /// no socket call can reach it again.
    fn leave(&mut self, id: AnnotationId, a: Annotation) {
        self.leaving.insert(id, (a, Instant::now()));
        while self.leaving.len() > MAX_LIVE {
            self.leaving.pop_first();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&AnnotationId, &Annotation)> {
        self.live.iter()
    }

    /// Whether a fade or slide is in flight, so the next frame must be drawn
    /// even with no damage. False whenever animations are off.
    pub fn animating(&self) -> bool {
        if !self.motion {
            return false;
        }
        let now = Instant::now();
        !self.leaving.is_empty()
            || self.live.values().any(|a| {
                now.duration_since(a.born) < ENTER
                    || a.changed.is_some_and(|c| now.duration_since(c) < CROSSFADE)
            })
    }
}

/// A title is one line of text, capped hard.
fn title_for(s: &str) -> String {
    text::sanitize_line(s).chars().take(MAX_TITLE).collect()
}

/// Progress `0.0..=1.0` of a transition that started at `since`.
fn progress(now: Instant, since: Instant, over: Duration) -> f64 {
    (now.duration_since(since).as_secs_f64() / over.as_secs_f64()).clamp(0.0, 1.0)
}

/// An annotation's fade and how much of its entrance slide is left, both
/// `0.0..=1.0`. Entering: cubic ease-out. Leaving (`gone`): cubic ease-in,
/// and no slide -- it fades where it stands.
fn fade(born: Instant, gone: Option<Instant>, now: Instant, motion: bool) -> (f32, f64) {
    if !motion {
        return (1.0, 0.0);
    }
    if let Some(gone) = gone {
        let t = progress(now, gone, EXIT);
        return ((1.0 - t * t * t) as f32, 0.0);
    }
    let t = progress(now, born, ENTER);
    let e = 1.0 - (1.0 - t).powi(3);
    (e as f32, 1.0 - e)
}

/// Build this output's annotation elements.
///
/// Returns front-to-back, like every other element list. The caller splices
/// these in below trusted UI and above the cursor. `output_loc` is the
/// output's origin in the global space; anchors arrive in global coordinates
/// and are drawn output-local. `behind` is everything that will be drawn
/// under the annotations, front to back: what a glass panel blurs. `None`
/// (or blur off in config, or a rotated output) draws the opaque fallback.
pub fn annotation_elements(
    renderer: &mut GlesRenderer,
    store: &mut AnnotationStore,
    output: &Output,
    output_loc: Point<i32, Logical>,
    config: &Config,
    behind: Option<&[AbyssRenderElement]>,
) -> Vec<AbyssRenderElement> {
    let now = Instant::now();
    store.motion = config.animations.motion();
    if !store.motion {
        store.leaving.clear();
    }
    store
        .leaving
        .retain(|_, (_, gone)| now.duration_since(*gone) < EXIT);
    if store.live.is_empty() && store.leaving.is_empty() {
        // Nothing to draw: let the textures and blur chains go.
        store.art.clear();
        store.blur.clear();
        return Vec::new();
    }
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let fractional = output.current_scale().fractional_scale();
    let scale = Scale::from(fractional);
    let logical: Size<i32, Logical> = mode.size.to_f64().to_logical(scale).to_i32_round();
    let dev = hud_font::raster_scale(fractional);
    let palette = HudPalette::new(&config.annotations);

    // Glass needs a blur to be configured at all, something to blur, and an
    // upright output (the mask works in `gl_FragCoord`, which a rotation
    // turns); otherwise the near-opaque fallback.
    let transform = output.current_transform();
    let mirrored = effects::fb_y_mirrored(transform);
    let mut glass = config.decoration.blur.mode != BlurMode::Off && behind.is_some() && mirrored.is_some();
    if glass && store.glass.is_none() && !store.glass_failed {
        match effects::compile_hud_glass(renderer) {
            Ok(p) => store.glass = Some(p),
            Err(err) => {
                tracing::warn!(?err, "compiling the annotation backdrop; panels drawn opaque");
                store.glass_failed = true;
            }
        }
    }
    glass &= store.glass.is_some();
    let fb_height = transform.transform_size(mode.size).h;
    let blur_cfg = Blur {
        size: GLASS_BLUR_SIZE,
        passes: GLASS_BLUR_PASSES,
        ..Blur::default()
    };

    // Split borrow: the maps are read while the caches are written.
    let AnnotationStore {
        live,
        leaving,
        art,
        blur,
        glass: program,
        motion,
        ..
    } = store;
    let motion = *motion;
    if !art.contains_key(output) {
        art.insert(output.clone(), BTreeMap::new());
    }
    let Some(arts) = art.get_mut(output) else {
        return Vec::new();
    };
    arts.retain(|id, _| live.contains_key(id) || leaving.contains_key(id));

    let mut elements = Vec::new();
    // Panels already placed on this output this frame, so the next one can
    // keep clear of them (COMP-18 §3, collision avoidance).
    let mut taken = Vec::new();
    let mut blurred = Vec::new();
    let all = live
        .iter()
        .map(|(id, a)| (id, a, None))
        .chain(leaving.iter().map(|(id, (a, gone))| (id, a, Some(*gone))));
    for (id, annotation, gone) in all {
        let Some(layout) = lay_out(annotation, logical, output_loc, &taken) else {
            continue;
        };
        taken.push(layout.panel);
        let key = ArtKey {
            card: layout.card.clone(),
            dev,
            palette,
            glass,
            region: layout.region,
            pick: layout.pick.as_ref().map(|p| p.rect),
            panel: layout.panel,
        };
        if arts.get(id).map(|a| a.key != key).unwrap_or(true) {
            let old = arts.remove(id);
            // A failed upload drops this one annotation rather than the frame.
            let parts: Option<Vec<Part>> = draw(&layout, &palette, glass, dev)
                .into_iter()
                .map(|p| {
                    upload(renderer, &p.raster, dev).map(|buffer| Part {
                        at: p.at,
                        buffer,
                        panel: p.panel,
                    })
                })
                .collect();
            let Some(parts) = parts else { continue };
            // Cross-fade from the old panel only for a text update, not for
            // a rebuild a scale change or a move forced.
            let fresh = annotation
                .changed
                .is_some_and(|c| motion && now.duration_since(c) < CROSSFADE);
            let prev = old
                .filter(|_| fresh)
                .and_then(|o| o.parts.into_iter().find(|p| p.panel));
            arts.insert(*id, Art { key, parts, prev });
        }
        let Some(a) = arts.get_mut(id) else { continue };

        let (alpha, slide_left) = fade(annotation.born, gone, now, motion);
        // The panel starts nearer its region and slides away into place.
        let (dir, k) = (layout.side.away(), -SLIDE * slide_left);
        let slide = Point::<f64, Logical>::from((dir.x as f64 * k, dir.y as f64 * k));
        let x = match annotation.changed {
            Some(c) if motion && a.prev.is_some() => progress(now, c, CROSSFADE) as f32,
            _ => 1.0,
        };
        if x >= 1.0 {
            a.prev = None;
        }
        let at = |p: &Part| {
            let loc = p.at.to_f64() + if p.panel { slide } else { Point::default() };
            loc.to_physical(scale)
        };
        let element = |p: &Part, k: f32| {
            AbyssRenderElement::Texture(TextureRenderElement::from_texture_buffer(
                at(p),
                &p.buffer,
                (k < 1.0).then_some(k.max(0.0)),
                None,
                None,
                Kind::Unspecified,
            ))
        };
        for part in &a.parts {
            if !part.panel {
                elements.push(element(part, alpha));
                continue;
            }
            elements.push(element(part, alpha * x));
            if let Some(prev) = &a.prev {
                elements.push(element(prev, alpha * (1.0 - x)));
            }
            // The glass, directly behind the panel's own texture.
            let (Some(program), Some(mirrored), Some(behind)) = (program.as_ref(), mirrored, behind) else {
                continue;
            };
            if !glass {
                continue;
            }
            let loc = (layout.panel.loc.to_f64() + slide)
                .to_physical(scale)
                .to_i32_round();
            let region: Rectangle<i32, Physical> =
                Rectangle::new(loc, layout.panel.size.to_f64().to_physical(scale).to_i32_round());
            let radius = (hud::RADIUS as f64 * fractional) as f32;
            let uniforms = effects::rounding_uniforms(region, fb_height, mirrored, radius);
            let key = BlurKey::Annotation(id.0);
            let store = blur.entry(output.clone()).or_default();
            if let Some(b) = store.element(
                renderer,
                output,
                &key,
                region,
                behind,
                &blur_cfg,
                scale,
                Some((program.clone(), uniforms)),
                0,
                None,
                None,
                alpha,
            ) {
                elements.push(AbyssRenderElement::Blur(b));
            }
            blurred.push(key);
        }
    }
    if let Some(store) = blur.get_mut(output) {
        store.retain(&blurred);
    }
    elements
}

/// Which side of its region a panel sits on, which decides the direction of
/// its entrance slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Right,
    Left,
    Below,
    Above,
    /// Unanchored: top-centre.
    Top,
}

impl Side {
    /// Unit step from the region toward the panel. The panel enters from the
    /// region's side, so it starts one slide's length back along this.
    fn away(self) -> Point<i32, Logical> {
        match self {
            Side::Right => (1, 0),
            Side::Left => (-1, 0),
            Side::Below | Side::Top => (0, 1),
            Side::Above => (0, -1),
        }
        .into()
    }

    fn stacked(self) -> bool {
        matches!(self, Side::Below | Side::Above)
    }
}

/// Where everything about one annotation goes on one output, output-local and
/// logical. Pure: computed every frame, and the tests draw from it directly.
#[derive(Debug, Clone)]
struct Layout {
    card: Card,
    panel: Rectangle<i32, Logical>,
    /// The part of the anchor that may be marked, `None` when there is no
    /// honest region to point at -- the panel is then drawn unmarked.
    region: Option<Rectangle<i32, Logical>>,
    pick: Option<Pick>,
    side: Side,
}

fn lay_out(
    a: &Annotation,
    output: Size<i32, Logical>,
    output_loc: Point<i32, Logical>,
    taken: &[Rectangle<i32, Logical>],
) -> Option<Layout> {
    let error = a.kind == AnnotationKind::Error;
    let Some(anchor) = a.anchor else {
        return unanchored(a, output, taken);
    };
    let local = |r: Rectangle<i32, Logical>| Rectangle::new(r.loc - output_loc, r.size);
    let region = drawable_region(local(anchor), output);
    // A pick is drawn only inside what is drawn of the anchor: one hanging off
    // the output goes unmarked rather than half-marked.
    let pick = a.pick.as_ref().and_then(|p| {
        let rect = local(p.rect);
        region.filter(|r| r.contains_rect(rect)).map(|_| Pick {
            rect,
            label: p.label.clone(),
        })
    });
    let chip = pick.as_ref().map(|p| p.label.as_str()).unwrap_or("");
    let cols = columns(chip, &a.title, &a.text, output.w, error);
    let mut card = Card::new(chip, &a.title, &a.text, cols, error);
    if card.is_empty() {
        return None;
    }
    // What the panel must not cover: the region, and the tab beside a pick.
    let target = region.unwrap_or_else(|| local(anchor));
    let blocked = match &pick {
        Some(p) => target.merge(tab_rect(p)),
        None => target,
    };
    let focus = pick.as_ref().map(|p| p.rect).unwrap_or(target);
    let mut size = card_size(&card);
    let (mut loc, mut side) = place(blocked, focus, pick.is_some(), size, output, taken);
    if side.stacked() && cols < STACK_MIN_COLS {
        // Stacked: widen to the stacked minimum (as far as the output
        // allows) and place again.
        let fit = (output.w.max(0) as usize).saturating_sub(hud::PAD_X * 2) / ADVANCE;
        let wide_cols = STACK_MIN_COLS.min(fit).max(cols);
        let wide = Card::new(chip, &a.title, &a.text, wide_cols, error)
            .at_least(wide_cols * ADVANCE + hud::PAD_X * 2);
        let wide_size = card_size(&wide);
        let (l, s) = place(blocked, focus, pick.is_some(), wide_size, output, taken);
        if s.stacked() {
            (card, size, loc, side) = (wide, wide_size, l, s);
        }
    }
    Some(Layout {
        card,
        panel: Rectangle::new(loc, size),
        region,
        pick,
        side,
    })
}

/// An unanchored note: top-centre, [`TOP_MARGIN`] down, stacking under any
/// note already there. Nothing to ring, nothing to point at.
fn unanchored(
    a: &Annotation,
    output: Size<i32, Logical>,
    taken: &[Rectangle<i32, Logical>],
) -> Option<Layout> {
    let error = a.kind == AnnotationKind::Error;
    let cols = columns("", &a.title, &a.text, output.w, error);
    let card = Card::new("", &a.title, &a.text, cols, error);
    if card.is_empty() {
        return None;
    }
    let size = card_size(&card);
    let x = ((output.w - size.w) / 2).max(0);
    let mut panel = Rectangle::new(Point::from((x, TOP_MARGIN)), size);
    // Bounded: each step moves strictly below one of `taken`.
    for _ in 0..=taken.len() {
        match taken
            .iter()
            .filter(|t| t.overlaps(panel))
            .map(|t| t.loc.y + t.size.h)
            .max()
        {
            Some(bottom) => panel.loc.y = bottom + STACK_GAP,
            None => break,
        }
    }
    Some(Layout {
        card,
        panel,
        region: None,
        pick: None,
        side: Side::Top,
    })
}

fn card_size(card: &Card) -> Size<i32, Logical> {
    let (w, h) = card.size();
    Size::from((w as i32, h as i32))
}

/// The column count for a panel on an output `output_w` logical pixels wide.
///
/// The widest the output allows -- a third of it, within [`MIN_COLS`] and
/// [`MAX_COLS`] -- and then narrowed while that costs no extra line, so a
/// three-line answer is three even lines rather than two full ones and a
/// straggler.
fn columns(chip: &str, title: &str, body: &str, output_w: i32, error: bool) -> usize {
    let room = (output_w.max(0) as usize / 3).saturating_sub(hud::PAD_X * 2) / ADVANCE;
    let widest = room.clamp(MIN_COLS, MAX_COLS);
    let rows = |cols: usize| {
        let c = Card::new(chip, title, body, cols, error);
        (c.title.len(), c.body.len())
    };
    let want = rows(widest);
    let mut cols = widest;
    while cols > MIN_COLS && rows(cols - 1) == want {
        cols -= 1;
    }
    cols
}

/// One raster of an annotation, where it goes (output-local, logical), and
/// whether it is the panel.
struct Piece {
    at: Point<i32, Logical>,
    raster: text::Raster,
    panel: bool,
}

/// Draw one annotation, front to back: the panel, then -- when there is an
/// honest region to point at -- the pick tab and wash, the leader, and the
/// rim. The panel alone says *what*; the rest says *about what*. All of it
/// is in the same pass, so it inherits capture-invisibility and its place
/// below trusted UI without anything new having to be true.
fn draw(l: &Layout, pal: &HudPalette, glass: bool, dev: usize) -> Vec<Piece> {
    let shadow = Point::from((hud::SHADOW_X as i32, hud::SHADOW_TOP as i32));
    let mut out = vec![Piece {
        at: l.panel.loc - shadow,
        raster: hud::panel(&l.card, pal, glass, dev),
        panel: true,
    }];
    let Some(region) = l.region else { return out };
    let mark = |at: Point<i32, Logical>, raster| Piece {
        at,
        raster,
        panel: false,
    };
    if let Some(p) = &l.pick {
        // The pick is the only gold: a solid tab with the letter in ink, and
        // a faint wash with a rim over the option itself.
        let t = tab_rect(p);
        let (tab, _) = hud::pill(
            &p.label,
            Face::Title,
            (0, TAB_PAD_X),
            pal.accent,
            None,
            pal.keyline,
            pal.ink,
            dev,
        );
        let m = hud::PILL_MARGIN as i32;
        out.push(mark(t.loc - Point::from((m, m)), tab));
        let w = wash_rect(p);
        out.push(mark(
            w.loc,
            hud::shape(
                w.size.w as usize,
                w.size.h as usize,
                WASH_RADIUS,
                Some(pal.pick_wash),
                Some((1.0, pal.pick_rim)),
                dev,
            ),
        ));
    }
    let focus = l.pick.as_ref().map(|p| p.rect).unwrap_or(region);
    if let Some(lead) = leader(region, focus, l.panel) {
        let r = LEADER_DOT / 2;
        out.push(mark(
            lead.dot - Point::from((r, r)),
            hud::shape(
                LEADER_DOT as usize,
                LEADER_DOT as usize,
                r as f32,
                Some(pal.leader_dot),
                None,
                dev,
            ),
        ));
        for run in lead.runs {
            out.push(mark(
                run.loc,
                hud::shape(
                    run.size.w as usize,
                    run.size.h as usize,
                    0.0,
                    Some(pal.leader),
                    None,
                    dev,
                ),
            ));
        }
    }
    let rr = (region.loc.x, region.loc.y, region.size.w, region.size.h);
    for part in hud::rim_parts(rr) {
        out.push(mark((part.0, part.1).into(), hud::rim_part(rr, part, pal, dev)));
    }
    out
}

/// The faint wash over a pick: the option grown by [`WASH_GROW`].
fn wash_rect(p: &Pick) -> Rectangle<i32, Logical> {
    let g = WASH_GROW;
    Rectangle::new(
        p.rect.loc - Point::from((g, g)),
        p.rect.size + Size::from((2 * g, 2 * g)),
    )
}

/// The tab beside a pick: left of the wash, level with the option's first
/// line. It may stick out of the anchor to the left; that is compositor
/// geometry keyed to a rectangle already inside it, not the caller's.
///
/// Never off the output: an option hard against the left edge (x near 0)
/// used to put the tab at negative x, half of it or all of it unseen. When
/// there is no room on the left the tab moves inside the wash, at the
/// option's left edge -- covering a sliver of the option beats a pick nobody
/// can see. `p` is output-local, so the output's left edge is x = 0.
fn tab_rect(p: &Pick) -> Rectangle<i32, Logical> {
    let (w, h) = hud::pill_size(&p.label, Face::Title, (0, TAB_PAD_X));
    let (w, h) = (w as i32, h as i32);
    let first = p.rect.size.h.min(Face::Title.line_h() as i32);
    let outside = wash_rect(p).loc.x - TAB_GAP - w;
    let x = if outside >= 0 {
        outside
    } else {
        p.rect.loc.x.max(0)
    };
    let y = p.rect.loc.y + (first - h) / 2;
    Rectangle::new((x, y).into(), (w, h).into())
}

/// A leader: the dot outside the region rim, and the one or two runs from it
/// to the panel's edge.
#[derive(Debug)]
struct Leader {
    dot: Point<i32, Logical>,
    runs: Vec<Rectangle<i32, Logical>>,
}

/// A 1px run between two points that share a row or a column.
fn run(a: Point<i32, Logical>, b: Point<i32, Logical>) -> Rectangle<i32, Logical> {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    Rectangle::new(
        (x0, y0).into(),
        ((a.x - b.x).abs().max(1), (a.y - b.y).abs().max(1)).into(),
    )
}

/// The leader from a region to its panel, or `None` when the panel sits
/// within [`LEADER_MIN`] of it (it is plainly beside its region, and a line
/// would only clutter) or overlaps it.
///
/// It leaves the region level with `focus` -- the pick, when there is one --
/// so it points at the answer, not at the middle of the question. Straight
/// when the panel spans that row (or column); otherwise one elbow, entering
/// the panel through its nearer edge.
fn leader(
    region: Rectangle<i32, Logical>,
    focus: Rectangle<i32, Logical>,
    panel: Rectangle<i32, Logical>,
) -> Option<Leader> {
    if region.size.w <= 0 || region.size.h <= 0 || panel.size.w <= 0 || panel.size.h <= 0 {
        return None;
    }
    let (rx0, ry0) = (region.loc.x, region.loc.y);
    let (rx1, ry1) = (rx0 + region.size.w, ry0 + region.size.h);
    let (px0, py0) = (panel.loc.x, panel.loc.y);
    let (px1, py1) = (px0 + panel.size.w, py0 + panel.size.h);
    // The dot's centre sits clear of the rim and its keyline.
    let off = hud::REGION_REACH + 1 + LEADER_DOT / 2;
    let edge = LEADER_DOT / 2;
    // Keep off the rim's rounded corners where there is room to.
    let inset = |lo: i32, hi: i32, v: i32| {
        let r = hud::REGION_RADIUS as i32;
        if hi - lo > 2 * r {
            v.clamp(lo + r, hi - r)
        } else {
            lo + (hi - lo) / 2
        }
    };
    let p = |x: i32, y: i32| Point::<i32, Logical>::from((x, y));
    // How far into a panel a bent leader enters, from its corner.
    let enter = hud::first_line_mid();
    if px0 >= rx1 || px1 <= rx0 {
        let right = px0 >= rx1;
        let gap = if right { px0 - rx1 } else { rx0 - px1 };
        if gap < LEADER_MIN {
            return None;
        }
        let y = inset(
            ry0,
            ry1,
            focus.loc.y + focus.size.h.min(Face::Title.line_h() as i32) / 2,
        );
        let (dot, start, near) = if right {
            (p(rx1 + off, y), rx1 + off + edge, px0)
        } else {
            (p(rx0 - off, y), rx0 - off - edge, px1)
        };
        let runs = if (py0 + 8..=py1 - 8).contains(&y) {
            vec![run(p(start, y), p(near, y))]
        } else {
            let x = if right { px0 + enter } else { px1 - enter };
            let to = if y < py0 { py0 } else { py1 };
            vec![run(p(start, y), p(x, y)), run(p(x, y), p(x, to))]
        };
        Some(Leader { dot, runs })
    } else if py0 >= ry1 || py1 <= ry0 {
        let below = py0 >= ry1;
        let gap = if below { py0 - ry1 } else { ry0 - py1 };
        if gap < LEADER_MIN {
            return None;
        }
        let x = inset(rx0, rx1, focus.loc.x + focus.size.w / 2);
        let (dot, start, near) = if below {
            (p(x, ry1 + off), ry1 + off + edge, py0)
        } else {
            (p(x, ry0 - off), ry0 - off - edge, py1)
        };
        let runs = if (px0 + 8..=px1 - 8).contains(&x) {
            vec![run(p(x, start), p(x, near))]
        } else {
            let y = if below { py0 + enter } else { py1 - enter };
            let to = if x < px0 { px0 } else { px1 };
            vec![run(p(x, start), p(x, y)), run(p(x, y), p(to, y))]
        };
        Some(Leader { dot, runs })
    } else {
        None
    }
}

/// What of a caller's region may actually be drawn: the region clipped to
/// this output, or `None` when nothing of it is on the output (the panel is
/// then drawn unmarked).
///
/// This used to cap the region at half the output per axis, against a
/// caller outlining the whole screen (ADR 0040 rationale). The cap trimmed
/// the rim of every tall or wide question and, worse, silently dropped a pick
/// that fell outside the trimmed part -- the one mark the human needed. The
/// region is drawn whole now; what still bounds it is the output itself and
/// [`clamp_anchor`] at the door. Panel size is capped separately, by
/// [`columns`]. NOTE for owner review (ADR 0040): this removes the
/// documented "cannot frame the whole screen" guard; the rim is a 1px
/// translucent outline, not a fill, so a full-screen frame obscures nothing.
fn drawable_region(
    r: Rectangle<i32, Logical>,
    output: Size<i32, Logical>,
) -> Option<Rectangle<i32, Logical>> {
    if r.size.w <= 0 || r.size.h <= 0 {
        return None;
    }
    r.intersection(Rectangle::new(Point::from((0, 0)), output))
}

/// Where a panel of `size` goes, beside `blocked` and as near `focus` as it
/// can get, and which side that is.
///
/// Four candidates -- right of the region, left, below, above -- each slid
/// along its edge to line up with `focus` and then kept on the output. A
/// candidate counts if it fits on the output without covering the region or a
/// panel already placed.
///
/// With a pick (`level`) the order is the rule: right, then left only if
/// right does not fit, then below, then above -- a side panel level with the
/// option, so the eye runs straight across from the answer to the reason.
/// Without one, of the candidates that fit, the one whose near edge is
/// closest to the focus's centre *across the gap* wins, ties going right,
/// below, left, above: that is half the focus's width for a side panel and
/// half its height for a stacked one, so a wide paragraph gets its answer
/// underneath and a narrow column gets it alongside.
///
/// If nothing fits clear of the other panels, overlap with them is allowed;
/// if nothing fits at all, the panel is clamped onto the output below the
/// region, which is where it always used to go.
fn place(
    blocked: Rectangle<i32, Logical>,
    focus: Rectangle<i32, Logical>,
    level: bool,
    size: Size<i32, Logical>,
    output: Size<i32, Logical>,
    taken: &[Rectangle<i32, Logical>],
) -> (Point<i32, Logical>, Side) {
    let (w, h) = (size.w, size.h);
    let (bx, by, br, bb) = (
        blocked.loc.x,
        blocked.loc.y,
        blocked.loc.x + blocked.size.w,
        blocked.loc.y + blocked.size.h,
    );
    let fit_x = |x: i32| x.min(output.w - w).max(0);
    let fit_y = |y: i32| y.min(output.h - h).max(0);
    // Level with the focus: a panel beside it puts the middle of its first
    // title line on the middle of the focus's first line; one above or below
    // starts at its left edge.
    let first = focus.size.h.min(Face::Title.line_h() as i32);
    let side_y = fit_y(focus.loc.y + first / 2 - hud::first_line_mid());
    let stack_x = fit_x(focus.loc.x.min(bx.max(0)));
    let right = (Point::from((br + GUTTER, side_y)), Side::Right);
    let left = (Point::from((bx - GUTTER - w, side_y)), Side::Left);
    let below = (Point::from((stack_x, bb + GUTTER)), Side::Below);
    let above = (Point::from((stack_x, by - GUTTER - h)), Side::Above);
    let candidates = if level {
        [right, left, below, above]
    } else {
        [right, below, left, above]
    };
    let screen = Rectangle::from_size(output);
    let centre: Point<i32, Logical> =
        Point::from((focus.loc.x + focus.size.w / 2, focus.loc.y + focus.size.h / 2));
    let reach = |(p, side): (Point<i32, Logical>, Side)| {
        if level {
            return 0;
        }
        if side.stacked() {
            (p.y - centre.y).max(centre.y - (p.y + h))
        } else {
            (p.x - centre.x).max(centre.x - (p.x + w))
        }
    };
    for clear_of_others in [true, false] {
        // `min_by_key` keeps the first of equal keys, so with a pick (every
        // key 0) this is simply the first candidate that fits.
        let best = candidates
            .iter()
            .copied()
            .filter(|(p, _)| {
                let r = Rectangle::new(*p, size);
                screen.contains_rect(r)
                    && !r.overlaps(blocked)
                    && (!clear_of_others || !taken.iter().any(|t| r.overlaps(*t)))
            })
            .min_by_key(|c| reach(*c));
        if let Some(c) = best {
            return c;
        }
    }
    let y = bb + GUTTER;
    if y + h > output.h {
        let above = by - GUTTER - h;
        if above >= 0 {
            return ((fit_x(bx), above).into(), Side::Above);
        }
        return ((fit_x(bx), (output.h - h).max(0)).into(), Side::Below);
    }
    ((fit_x(bx), y).into(), Side::Below)
}

/// Upload a raster to a texture, ready to be drawn at buffer scale `bs`.
///
/// `None` on any GL failure: an annotation is cosmetic and must never be able
/// to take a frame down with it.
pub fn upload(
    renderer: &mut GlesRenderer,
    raster: &text::Raster,
    bs: usize,
) -> Option<TextureBuffer<GlesTexture>> {
    // `Abgr8888` is RGBA in memory order on a little-endian host, which is how
    // `hud` lays its rasters out.
    let texture: GlesTexture = renderer
        .import_memory(&raster.px, Fourcc::Abgr8888, (raster.w, raster.h).into(), false)
        .ok()?;
    Some(TextureBuffer::from_texture(
        renderer,
        texture,
        bs as i32,
        Transform::Normal,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn pick(r: Rectangle<i32, Logical>, label: &str) -> Option<Pick> {
        Some(Pick {
            rect: r,
            label: label.into(),
        })
    }

    const ANSWER: AnnotationKind = AnnotationKind::Answer;

    fn add(s: &mut AnnotationStore, owner: u64, text: &str) -> Option<AnnotationId> {
        s.create(owner, Some(rect(0, 0, 10, 10)), ANSWER, "", text, None)
    }

    #[test]
    fn handles_are_never_reused() {
        let mut s = AnnotationStore::default();
        let a = add(&mut s, 1, "one").unwrap();
        assert!(s.destroy(1, a));
        let b = add(&mut s, 1, "two").unwrap();
        assert_ne!(a, b, "a destroyed handle must not come back");
        assert!(!s.destroy(1, a), "the stale handle must fail closed");
    }

    #[test]
    fn a_leaving_annotation_cannot_be_addressed() {
        // The exit fade keeps it for drawing only.
        let mut s = AnnotationStore::default();
        let a = add(&mut s, 1, "one").unwrap();
        assert!(s.destroy(1, a));
        assert!(!s.update(1, a, "", "back"));
        assert!(!s.destroy(1, a));
        assert_eq!(s.clear_for(1), 0);
        assert!(s.is_empty());
        assert_eq!(s.iter().count(), 0);
    }

    #[test]
    fn the_leaving_set_is_capped() {
        let mut s = AnnotationStore::default();
        for _ in 0..3 {
            for _ in 0..MAX_LIVE {
                add(&mut s, 1, "x").unwrap();
            }
            s.clear_for(1);
        }
        assert!(s.leaving.len() <= MAX_LIVE);
    }

    #[test]
    fn text_is_reduced_at_the_door() {
        // Whatever the caller sends, what is stored is already sanitised --
        // there is no path that renders the raw string.
        let mut s = AnnotationStore::default();
        let id = s
            .create(
                1,
                Some(rect(0, 0, 10, 10)),
                ANSWER,
                "t\u{1b}i\ntle",
                "a\u{1b}[2Jb\u{0}c",
                None,
            )
            .unwrap();
        let a = s.iter().next().unwrap().1;
        assert_eq!(a.text, "a[2Jbc");
        assert_eq!(a.title, "ti tle", "a title is one line");
        assert!(s.update(1, id, "", "\u{7}x"));
        let a = s.iter().next().unwrap().1;
        assert_eq!((a.title.as_str(), a.text.as_str()), ("", "x"));
    }

    #[test]
    fn a_title_is_capped() {
        let mut s = AnnotationStore::default();
        s.create(1, Some(rect(0, 0, 10, 10)), ANSWER, &"t".repeat(500), "", None);
        assert_eq!(s.iter().next().unwrap().1.title.len(), MAX_TITLE);
    }

    #[test]
    fn update_on_an_unknown_handle_is_false_not_a_new_annotation() {
        let mut s = AnnotationStore::default();
        assert!(!s.update(1, AnnotationId(42), "", "hi"));
        assert!(s.is_empty());
    }

    #[test]
    fn a_caller_can_only_address_its_own_handles() {
        let mut s = AnnotationStore::default();
        let mine = add(&mut s, 1, "mine").unwrap();
        let theirs = add(&mut s, 2, "theirs").unwrap();
        assert!(!s.update(2, mine, "", "hijack"));
        assert!(!s.destroy(2, mine));
        assert_eq!(s.clear_for(2), 1, "only conn 2's own annotation goes");
        assert!(!s.destroy(1, theirs));
        assert!(s.destroy(1, mine));
        assert!(s.is_empty());
    }

    #[test]
    fn the_live_cap_holds() {
        let mut s = AnnotationStore::default();
        for _ in 0..MAX_LIVE {
            assert!(add(&mut s, 1, "x").is_some());
        }
        assert!(add(&mut s, 1, "x").is_none());
        assert_eq!(s.clear_for(1), MAX_LIVE);
        assert!(add(&mut s, 1, "x").is_some());
    }

    #[test]
    fn a_pick_must_lie_inside_its_anchor() {
        // ADR 0054: the marker may only fall on what the caller was already
        // allowed to mark. Outside -- even by a pixel -- it is dropped and
        // the text is kept.
        let mut s = AnnotationStore::default();
        let anchor = Some(rect(100, 100, 300, 200));
        s.create(1, anchor, ANSWER, "", "x", pick(rect(110, 150, 200, 20), "b"));
        s.create(1, anchor, ANSWER, "", "x", pick(rect(110, 150, 291, 20), "B"));
        s.create(1, anchor, ANSWER, "", "x", pick(rect(5000, 5000, 20, 20), "B"));
        s.create(1, anchor, ANSWER, "", "x", pick(rect(110, 150, 0, 20), "B"));
        let picks: Vec<_> = s.iter().map(|(_, a)| a.pick.clone()).collect();
        assert_eq!(picks.len(), 4, "a dropped pick keeps its annotation");
        assert_eq!(
            picks[0],
            Some(Pick {
                rect: rect(110, 150, 200, 20),
                label: "B".into()
            })
        );
        assert!(picks[1..].iter().all(Option::is_none));
    }

    #[test]
    fn an_error_or_an_unanchored_note_never_keeps_a_pick() {
        // Error: no gold. Unanchored: nothing for the pick to lie inside.
        let mut s = AnnotationStore::default();
        let anchor = rect(100, 100, 300, 200);
        let p = pick(rect(110, 150, 200, 20), "B");
        s.create(1, Some(anchor), AnnotationKind::Error, "no", "x", p.clone());
        s.create(1, None, ANSWER, "t", "x", p);
        assert!(s.iter().all(|(_, a)| a.pick.is_none()));
    }

    #[test]
    fn an_unanchored_note_sits_top_centre_without_marks() {
        let out: Size<i32, Logical> = OUT.into();
        let mut s = AnnotationStore::default();
        s.create(1, None, AnnotationKind::Error, "no answer", "rate limited", None);
        s.create(1, None, ANSWER, "second", "below the first", None);
        let mut taken = Vec::new();
        for (_, a) in s.iter() {
            let l = lay_out(a, out, (0, 0).into(), &taken).unwrap();
            assert!(l.region.is_none() && l.pick.is_none());
            let centre = l.panel.loc.x + l.panel.size.w / 2;
            assert!((centre - out.w / 2).abs() <= 1, "{:?}", l.panel);
            assert!(taken
                .iter()
                .all(|t: &Rectangle<i32, Logical>| !t.overlaps(l.panel)));
            let pieces = draw(&l, &HudPalette::default(), true, 1);
            assert_eq!(pieces.len(), 1, "the panel and nothing else");
            taken.push(l.panel);
        }
        assert_eq!(taken[0].loc.y, TOP_MARGIN);
    }

    #[test]
    fn a_pick_label_is_one_or_two_letters_or_digits() {
        assert_eq!(pick_label("b").as_deref(), Some("B"));
        assert_eq!(pick_label("12").as_deref(), Some("12"));
        for bad in ["", "ABC", "é", " A", "A)", "\u{1b}"] {
            assert_eq!(pick_label(bad), None, "{bad:?}");
        }
        // The store checks again, whatever the door did.
        let mut s = AnnotationStore::default();
        s.create(
            1,
            Some(rect(0, 0, 100, 100)),
            ANSWER,
            "",
            "x",
            pick(rect(1, 1, 5, 5), "<>"),
        );
        assert!(s.iter().next().unwrap().1.pick.is_none());
    }

    #[test]
    fn a_tall_region_is_drawn_whole_and_keeps_its_pick() {
        // Bug A: a 900-tall question on a 1600x900 output used to be capped
        // at 450 tall, and a pick in the lower half silently vanished.
        let out: Size<i32, Logical> = (1600, 900).into();
        let anchor = rect(100, 40, 600, 820);
        let pick_r = rect(120, 780, 200, 20);
        assert_eq!(drawable_region(anchor, out), Some(anchor));
        let mut s = AnnotationStore::default();
        s.create(1, Some(anchor), ANSWER, "Paris", "", pick(pick_r, "B"))
            .unwrap();
        let a = s.iter().next().unwrap().1;
        let l = lay_out(a, out, (0, 0).into(), &[]).unwrap();
        assert_eq!(l.region, Some(anchor));
        assert_eq!(l.pick.map(|p| p.rect), Some(pick_r));
    }

    #[test]
    fn a_pick_tab_never_leaves_the_output() {
        // Bug B: an option at x = 16 put the tab at negative x.
        for x in [0, 4, 16] {
            let p = Pick {
                rect: rect(x, 200, 120, 16),
                label: "A".into(),
            };
            let t = tab_rect(&p);
            assert!(t.loc.x >= 0, "x={x}: {t:?}");
        }
        // With room on the left it stays outside the option, as before.
        let p = Pick {
            rect: rect(400, 200, 120, 16),
            label: "A".into(),
        };
        assert!(tab_rect(&p).loc.x + tab_rect(&p).size.w <= 400);
    }

    #[test]
    fn the_leader_is_drawn_only_across_a_real_gap() {
        // Beside the region, within LEADER_MIN: no line.
        assert!(leader(
            rect(100, 100, 200, 100),
            rect(100, 140, 50, 16),
            rect(318, 120, 200, 80)
        )
        .is_none());
        // Far to the right, level with the focus: one straight run, and the
        // dot outside the rim.
        let l = leader(
            rect(100, 100, 200, 100),
            rect(100, 140, 50, 16),
            rect(400, 120, 200, 80),
        )
        .unwrap();
        assert_eq!(l.runs.len(), 1, "{l:?}");
        assert!(l.dot.x > 300 + hud::REGION_REACH, "{l:?}");
        assert_eq!(l.runs[0].size.h, 1);
        assert_eq!(l.runs[0].loc.x + l.runs[0].size.w, 400);
        // Below and off to the side: one elbow, every run on an axis.
        let l = leader(
            rect(100, 100, 40, 40),
            rect(100, 100, 40, 40),
            rect(400, 300, 200, 60),
        )
        .unwrap();
        assert_eq!(l.runs.len(), 2, "{l:?}");
        for r in &l.runs {
            assert!(r.size.w == 1 || r.size.h == 1, "{l:?}");
        }
        // Overlapping, which only the last-resort placement produces: none.
        assert!(leader(rect(0, 0, 40, 40), rect(0, 0, 40, 40), rect(10, 10, 40, 10)).is_none());
        // Degenerate input is refused rather than drawn.
        assert!(leader(rect(0, 0, 0, 0), rect(0, 0, 0, 0), rect(0, 300, 10, 10)).is_none());
    }

    #[test]
    fn a_region_is_clipped_to_the_output_and_nothing_more() {
        let out: Size<i32, Logical> = (1920, 1080).into();
        let r = drawable_region(rect(-500, -500, 9000, 9000), out).unwrap();
        assert_eq!((r.loc.x, r.loc.y, r.size.w, r.size.h), (0, 0, 1920, 1080));
        let r = drawable_region(rect(300, 200, 400, 60), out).unwrap();
        assert_eq!((r.loc.x, r.loc.y, r.size.w, r.size.h), (300, 200, 400, 60));
        assert!(drawable_region(rect(5000, 5000, 100, 100), out).is_none());
    }

    #[test]
    fn an_absurd_anchor_is_reduced_at_the_door() {
        // The anchor is drawn, not just used for placement, so a socket
        // caller must not be able to overflow the additions in `rim_parts`,
        // `leader` and `place`.
        let mut store = AnnotationStore::default();
        let huge = Rectangle::new(
            Point::from((i32::MAX, i32::MIN)),
            Size::from((i32::MAX, i32::MAX)),
        );
        let id = store
            .create(1, Some(huge), ANSWER, "", "hi", pick(huge, "A"))
            .unwrap();
        let a = &store.live[&id];
        let anchor = a.anchor.unwrap();
        assert_eq!(anchor.loc.x, MAX_COORD);
        assert_eq!(anchor.loc.y, -MAX_COORD);
        assert_eq!(anchor.size.w, MAX_COORD);
        assert!(anchor.loc.x.checked_add(anchor.size.w).is_some());
        // Both clamp the same way, so the pick is still inside.
        assert_eq!(a.pick.as_ref().map(|p| p.rect), Some(anchor));
    }

    const OUT: (i32, i32) = (1920, 1080);

    fn placed(
        blocked: Rectangle<i32, Logical>,
        size: (i32, i32),
        taken: &[Rectangle<i32, Logical>],
    ) -> Rectangle<i32, Logical> {
        let size = Size::from(size);
        Rectangle::new(place(blocked, blocked, false, size, OUT.into(), taken).0, size)
    }

    #[test]
    fn a_narrow_region_gets_its_panel_alongside() {
        let r = rect(300, 300, 200, 300);
        let p = placed(r, (320, 120), &[]);
        assert_eq!(p.loc.x, 500 + GUTTER, "right of the region");
        assert!(!p.overlaps(r));
    }

    #[test]
    fn a_wide_region_gets_its_panel_underneath() {
        let r = rect(100, 300, 600, 80);
        let p = placed(r, (320, 120), &[]);
        assert_eq!(p.loc.y, 380 + GUTTER, "below the region");
        assert_eq!(p.loc.x, 100, "starting at its left edge");
    }

    #[test]
    fn a_region_at_the_right_edge_gets_its_panel_to_the_left() {
        let r = rect(1700, 200, 200, 600);
        let p = placed(r, (320, 120), &[]);
        assert_eq!(p.loc.x + 320 + GUTTER, 1700);
    }

    #[test]
    fn a_region_at_the_bottom_gets_its_panel_above() {
        let r = rect(100, 900, 600, 170);
        let p = placed(r, (320, 120), &[]);
        assert_eq!(p.loc.y + 120 + GUTTER, 900);
    }

    #[test]
    fn a_pick_pulls_a_side_panel_level_with_it() {
        let r = rect(300, 300, 200, 300);
        let focus = rect(310, 480, 150, 18);
        let (at, side) = place(r, focus, true, (320, 120).into(), OUT.into(), &[]);
        assert_eq!(side, Side::Right);
        assert_eq!(at.y + hud::first_line_mid(), 480 + 9, "first lines level");
    }

    #[test]
    fn with_a_pick_left_comes_before_below() {
        // A wide region: without a pick the panel goes underneath, but with
        // one the order is right, left, below, above.
        let r = rect(300, 300, 1300, 80);
        let size: Size<i32, Logical> = (260, 120).into();
        let (_, side) = place(r, r, false, size, OUT.into(), &[]);
        assert_eq!(side, Side::Below);
        let (_, side) = place(r, rect(310, 320, 80, 16), true, size, OUT.into(), &[]);
        assert_eq!(side, Side::Right);
        let r = rect(300, 300, 1600, 80);
        let (at, side) = place(r, rect(310, 320, 80, 16), true, size, OUT.into(), &[]);
        assert_eq!(side, Side::Left);
        assert_eq!(at.x + size.w + GUTTER, 300);
    }

    #[test]
    fn a_stacked_panel_is_at_least_the_stacked_minimum_wide() {
        let mut s = AnnotationStore::default();
        s.create(
            1,
            Some(rect(100, 300, 1700, 80)),
            ANSWER,
            "Short",
            "A few words.",
            None,
        );
        let a = s.iter().next().unwrap().1;
        let l = lay_out(a, OUT.into(), (0, 0).into(), &[]).unwrap();
        assert!(l.side.stacked(), "{:?}", l.side);
        assert!(
            l.panel.size.w as usize >= STACK_MIN_COLS * ADVANCE,
            "{:?}",
            l.panel
        );
    }

    #[test]
    fn panels_keep_clear_of_each_other() {
        let r = rect(300, 300, 200, 300);
        let first = placed(r, (320, 120), &[]);
        let second = placed(r, (320, 120), &[first]);
        assert!(!second.overlaps(first), "{first:?} {second:?}");
        assert!(!second.overlaps(r));
    }

    #[test]
    fn with_no_room_anywhere_the_panel_is_clamped_on_screen() {
        let out: Size<i32, Logical> = (800, 60).into();
        let (p, _) = place(
            rect(0, 0, 10, 10),
            rect(0, 0, 10, 10),
            false,
            (200, 400).into(),
            out,
            &[],
        );
        assert_eq!(p, (0, 0).into());
    }

    #[test]
    fn placement_is_output_relative() {
        let mut s = AnnotationStore::default();
        s.create(1, Some(rect(2020, 100, 200, 300)), ANSWER, "", "hello", None);
        let a = s.iter().next().unwrap().1;
        let l = lay_out(a, OUT.into(), (1920, 0).into(), &[]).unwrap();
        assert_eq!(l.region, Some(rect(100, 100, 200, 300)));
        assert_eq!(l.panel.loc.x, 300 + GUTTER);
    }

    #[test]
    fn columns_follow_the_output_and_balance_the_lines() {
        assert_eq!(
            columns("", "", &"word ".repeat(4), 1920, false),
            MIN_COLS,
            "short text never widens the panel"
        );
        let long = "word ".repeat(60);
        assert_eq!(
            columns("", "", &long, 1920, false),
            MAX_COLS.min(columns("", "", &long, 9999, false))
        );
        assert_eq!(
            columns("", "", &long, 600, false),
            MIN_COLS,
            "a small output gets the narrowest"
        );
        // Balanced: dropping one more column would cost a line.
        let c = columns("", "", &long, 1920, false);
        let rows = |c| text::wrap(&long, c).len();
        assert!(c == MIN_COLS || rows(c - 1) > rows(c));
    }

    #[test]
    fn motion_eases_in_and_out_and_is_off_when_animations_are() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(fade(t0, None, at(0), true), (0.0, 1.0));
        let (a, s) = fade(t0, None, at(110), true);
        assert!(a > 0.5 && s < 0.5, "ease-out is past halfway at half time");
        assert_eq!(fade(t0, None, at(500), true), (1.0, 0.0));
        let (a, _) = fade(t0, Some(at(1000)), at(1080), true);
        assert!(a > 0.5, "ease-in is still mostly there at half time");
        assert_eq!(fade(t0, Some(at(1000)), at(2000), true).0, 0.0);
        assert_eq!(fade(t0, None, at(0), false), (1.0, 0.0));
    }

    // ---------------------------------------------------------- scene dump
    //
    // `ABYSS_DUMP_ANNOTATIONS=<dir> cargo test -p ec-abyss-render annotation_scenes`
    // writes each scene below as a PPM: a fake page, and every raster `draw`
    // produces for it composited in place, with a CPU stand-in for the
    // glass (box-blurred, saturated 140%, masked). It is how placement and
    // styling are judged -- by looking -- and without the variable set it
    // still runs every scene through layout and drawing and checks the
    // invariants.

    /// (anchor, kind, title, text, pick): one `annotation_create`.
    type Note = (
        Option<Rectangle<i32, Logical>>,
        AnnotationKind,
        &'static str,
        &'static str,
        Option<Pick>,
    );

    struct Scene {
        name: &'static str,
        out: (i32, i32),
        dev: usize,
        /// Page background and text colours.
        bg: [u8; 3],
        ink: [u8; 3],
        /// Flat blocks under the text: (x, y, w, h, colour).
        blocks: Vec<(i32, i32, i32, i32, [u8; 3])>,
        /// Page text: (x, y, line), so the panel has something real to sit
        /// over.
        page: Vec<(i32, i32, &'static str)>,
        notes: Vec<Note>,
        glass: bool,
        /// A region selection in progress: (rect, cursor, pressed).
        select: Option<Drag>,
    }

    /// (rect, cursor, pressed): a selector scene.
    type Drag = (Rectangle<i32, Logical>, Point<i32, Logical>, bool);

    const BASE: [u8; 3] = [0x0b, 0x09, 0x06]; // STYLE.md base #0b0906
    const GREY: [u8; 3] = [200, 204, 210];

    fn scene(name: &'static str, page: Vec<(i32, i32, &'static str)>, notes: Vec<Note>) -> Scene {
        Scene {
            name,
            out: OUT,
            dev: 1,
            bg: BASE,
            ink: GREY,
            blocks: Vec::new(),
            page,
            notes,
            glass: true,
            select: None,
        }
    }

    fn quiz() -> Vec<(i32, i32, &'static str)> {
        vec![
            (420, 300, "3. Which planet in the solar system is the largest?"),
            (440, 330, "A) Mars"),
            (440, 352, "B) Venus"),
            (440, 374, "C) Jupiter"),
            (440, 396, "D) Mercury"),
            (420, 440, "The correct answer is D."),
        ]
    }

    fn prose() -> Vec<(i32, i32, &'static str)> {
        let mut v = Vec::new();
        for i in 0..6 {
            v.push((160, 500 + i * 22, "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua."));
        }
        v
    }

    const JUPITER: &str = "Jupiter is the largest planet, over twice the mass of all the others combined.";

    fn scenes() -> Vec<Scene> {
        let quiz_anchor = Some(rect(412, 292, 440, 128));
        let pick_c = pick(rect(440, 374, 90, 16), "C");
        let jupiter = (quiz_anchor, ANSWER, "Jupiter", JUPITER, pick_c.clone());
        vec![
            scene("quiz", quiz(), vec![jupiter.clone()]),
            Scene {
                out: (1280, 720),
                dev: 2,
                ..scene("quiz-2x", quiz(), vec![jupiter.clone()])
            },
            Scene {
                glass: false,
                ..scene("quiz-blur-off", quiz(), vec![jupiter.clone()])
            },
            scene(
                "low-confidence",
                quiz(),
                vec![(quiz_anchor, ANSWER, "Jupiter", "Probably Jupiter, by mass and by radius. (low confidence)", pick_c.clone())],
            ),
            scene(
                "prose",
                prose(),
                vec![(Some(rect(152, 492, 1060, 140)), ANSWER, "It asks for consent", "The paragraph is placeholder Latin; it carries no meaning beyond showing where body text goes in a layout.", None)],
            ),
            scene(
                "column-right",
                vec![(1560, 300, "Sidebar item one"), (1560, 322, "Sidebar item two"), (1560, 344, "Sidebar item three")],
                vec![(Some(rect(1552, 292, 300, 72)), ANSWER, "", "A list of three sidebar entries with no further context.", None)],
            ),
            scene(
                "two-panels",
                quiz(),
                vec![
                    (quiz_anchor, ANSWER, "First", "The first of two answers about the same region.", None),
                    (quiz_anchor, ANSWER, "Second", "The second answer has to find somewhere else to go.", None),
                ],
            ),
            scene(
                "failure",
                vec![],
                vec![(Some(rect(64, 64, 480, 96)), AnnotationKind::Error, "no answer", "claude: rate limited", None)],
            ),
            scene(
                "error-unanchored",
                quiz(),
                vec![
                    (None, AnnotationKind::Error, "No answer", "The model did not reply in time. (timeout after 30s)", None),
                    (None, ANSWER, "Unanchored", "A note about the whole screen sits top-centre, with no rim and no leader.", None),
                ],
            ),
            // Bug A: a question taller than half the output, pick near the
            // bottom. The rim must run the full height and the pick show.
            scene(
                "tall-region",
                vec![(420, 120, "1. A long question"), (440, 860, "D) The answer near the bottom")],
                vec![(Some(rect(412, 100, 600, 800)), ANSWER, "Bottom", "The pick sits below the old half-height cap.", pick(rect(440, 856, 240, 16), "D"))],
            ),
            // Bug B: options hard against the left edge of the output.
            scene(
                "left-edge-pick",
                vec![(16, 300, "Q. Which?"), (16, 330, "A) Left"), (16, 352, "B) Edge")],
                vec![(Some(rect(8, 292, 300, 80)), ANSWER, "Edge", "The tab stays on screen.", pick(rect(16, 330, 80, 16), "A"))],
            ),
            // A wide question on the right half: the panel must go left.
            scene(
                "pick-goes-left",
                vec![(1100, 300, "4. Pick the odd one out of these four words:"), (1120, 330, "A) Apple"), (1120, 352, "B) Pear"), (1120, 374, "C) Carrot")],
                vec![(Some(rect(1092, 292, 820, 104)), ANSWER, "Carrot", "The only vegetable; the rest are fruit.", pick(rect(1120, 374, 100, 16), "C"))],
            ),
            Scene {
                bg: [0x3a, 0x22, 0x10],
                ink: [0xf0, 0xd8, 0xb0],
                blocks: vec![
                    (360, 240, 620, 260, [0x7a, 0x3c, 0x14]),
                    (1000, 200, 500, 400, [0xc0, 0x6a, 0x1e]),
                    (0, 760, 1920, 320, [0x55, 0x30, 0x12]),
                ],
                ..scene("warm-base", quiz(), vec![jupiter.clone()])
            },
            Scene {
                bg: [0xff, 0xff, 0xff],
                ink: [0x20, 0x20, 0x24],
                blocks: vec![(0, 0, 1920, 56, [0xf1, 0xf3, 0xf5]), (1000, 260, 420, 240, [0xe8, 0xf0, 0xfe])],
                ..scene("white-page", quiz(), vec![jupiter.clone()])
            },
            Scene {
                select: Some((rect(412, 292, 412, 128), Point::from((824, 420)), true)),
                ..scene("selector", quiz(), vec![])
            },
            Scene {
                select: Some((rect(0, 0, 0, 0), Point::from((900, 500)), false)),
                ..scene("selector-hint", quiz(), vec![])
            },
        ]
    }

    /// Premultiplied `src` over an opaque RGB canvas.
    fn over(dst: &mut [u8], src: &[u8]) {
        let a = src[3] as u32;
        for k in 0..3 {
            dst[k] = (src[k] as u32 + dst[k] as u32 * (255 - a) / 255).min(255) as u8;
        }
    }

    /// Paint a raster (device pixels) at a logical point.
    fn blit(img: &mut [u8], w: usize, h: usize, dev: usize, at: Point<i32, Logical>, r: &text::Raster) {
        for y in 0..r.h as usize {
            for x in 0..r.w as usize {
                let dx = at.x as isize * dev as isize + x as isize;
                let dy = at.y as isize * dev as isize + y as isize;
                if dx < 0 || dy < 0 || dx as usize >= w || dy as usize >= h {
                    continue;
                }
                let s = &r.px[(y * r.w as usize + x) * 4..][..4];
                over(&mut img[(dy as usize * w + dx as usize) * 3..][..3], s);
            }
        }
    }

    /// The CPU stand-in for the glass: three box blurs (about the
    /// 28px kernel), saturated 140% about Rec.709 luma, inside the panel's
    /// rounded rectangle.
    fn glass(img: &mut [u8], w: usize, h: usize, dev: usize, panel: Rectangle<i32, Logical>) {
        let d = dev as i32;
        let (x0, y0) = (
            (panel.loc.x * d).max(0) as usize,
            (panel.loc.y * d).max(0) as usize,
        );
        let x1 = (((panel.loc.x + panel.size.w) * d) as usize).min(w);
        let y1 = (((panel.loc.y + panel.size.h) * d) as usize).min(h);
        let rad = 8 * dev as isize;
        // A window around the panel, wide enough for three passes to reach.
        let pad = 3 * rad as usize;
        let (wx0, wy0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
        let (wx1, wy1) = ((x1 + pad).min(w), (y1 + pad).min(h));
        let (ww, wh) = (wx1 - wx0, wy1 - wy0);
        let mut win: Vec<[f32; 3]> = (wy0..wy1)
            .flat_map(|y| (wx0..wx1).map(move |x| (x, y)))
            .map(|(x, y)| {
                let p = &img[(y * w + x) * 3..][..3];
                [p[0] as f32, p[1] as f32, p[2] as f32]
            })
            .collect();
        // Three separable box passes approximate the GPU's Gaussian-ish
        // dual-filter blur; edges clamp.
        let pass = |win: &mut Vec<[f32; 3]>, horizontal: bool| {
            let (n, m) = if horizontal { (wh, ww) } else { (ww, wh) };
            let at = |i: usize, j: usize| if horizontal { i * ww + j } else { j * ww + i };
            let mut out = win.clone();
            for i in 0..n {
                for j in 0..m {
                    let mut acc = [0f32; 3];
                    for k in -rad..=rad {
                        let jj = (j as isize + k).clamp(0, m as isize - 1) as usize;
                        let p = win[at(i, jj)];
                        acc = [acc[0] + p[0], acc[1] + p[1], acc[2] + p[2]];
                    }
                    let n = (2 * rad + 1) as f32;
                    out[at(i, j)] = [acc[0] / n, acc[1] / n, acc[2] / n];
                }
            }
            *win = out;
        };
        for _ in 0..3 {
            pass(&mut win, true);
            pass(&mut win, false);
        }
        let buf: Vec<[f32; 3]> = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .map(|(x, y)| win[(y - wy0) * ww + (x - wx0)])
            .collect();
        let b = (
            (panel.loc.x * d) as f32,
            (panel.loc.y * d) as f32,
            (panel.size.w * d) as f32,
            (panel.size.h * d) as f32,
        );
        let mut i = 0;
        for y in y0..y1 {
            for x in x0..x1 {
                let c = buf[i];
                i += 1;
                let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                let k = cov_box(x as f32 + 0.5, y as f32 + 0.5, b, hud::RADIUS * dev as f32);
                let dst = &mut img[(y * w + x) * 3..][..3];
                for j in 0..3 {
                    let s = (l + (c[j] - l) * 1.4).clamp(0.0, 255.0);
                    dst[j] = (s * k + dst[j] as f32 * (1.0 - k)).round() as u8;
                }
            }
        }
    }

    fn cov_box(x: f32, y: f32, (bx, by, bw, bh): (f32, f32, f32, f32), r: f32) -> f32 {
        let (hw, hh) = (bw * 0.5, bh * 0.5);
        let qx = (x - bx - hw).abs() - (hw - r);
        let qy = (y - by - hh).abs() - (hh - r);
        let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
        (0.5 - d).clamp(0.0, 1.0)
    }

    #[test]
    fn annotation_scenes() {
        let dir = std::env::var_os("ABYSS_DUMP_ANNOTATIONS");
        let pal = HudPalette::default();
        for scene in scenes() {
            let out: Size<i32, Logical> = scene.out.into();
            let mut store = AnnotationStore::default();
            for (anchor, kind, title, body, p) in &scene.notes {
                store.create(1, *anchor, *kind, title, body, p.clone());
            }
            let mut taken = Vec::new();
            // (panel rect, pieces)
            let mut drawn = Vec::new();
            for (_, a) in store.iter() {
                let l = lay_out(a, out, (0, 0).into(), &taken).expect("laid out");
                let screen = Rectangle::from_size(out);
                assert!(
                    screen.contains_rect(l.panel),
                    "{}: panel off screen {:?}",
                    scene.name,
                    l.panel
                );
                if let Some(r) = l.region {
                    assert!(!l.panel.overlaps(r), "{}: panel covers its region", scene.name);
                }
                if let Some(p) = &l.pick {
                    assert!(
                        !l.panel.overlaps(tab_rect(p)),
                        "{}: panel covers the tab",
                        scene.name
                    );
                    assert!(tab_rect(p).loc.x >= 0, "{}: tab off screen", scene.name);
                }
                assert!(
                    taken
                        .iter()
                        .all(|t: &Rectangle<i32, Logical>| !t.overlaps(l.panel)),
                    "{}: panels collide",
                    scene.name
                );
                taken.push(l.panel);
                drawn.push((l.panel, draw(&l, &pal, scene.glass, scene.dev)));
            }
            // The accent ledger: with no pick, nothing drawn is gold.
            if scene.notes.iter().all(|n| n.4.is_none()) {
                for (_, pieces) in &drawn {
                    for p in pieces {
                        assert!(
                            !p.raster.px.chunks(4).any(is_gold),
                            "{}: gold with no pick",
                            scene.name
                        );
                    }
                }
            }
            let select = scene.select.map(|(r, cursor, pressed)| {
                crate::select::sketch(pressed.then_some(r), cursor, out, &pal, scene.dev)
            });
            let Some(dir) = &dir else { continue };
            let dev = scene.dev;
            let (w, h) = (scene.out.0 as usize * dev, scene.out.1 as usize * dev);
            let mut img = vec![0u8; w * h * 3];
            for px in img.chunks_mut(3) {
                px.copy_from_slice(&scene.bg);
            }
            for &(bx, by, bw, bh, c) in &scene.blocks {
                for y in (by as usize * dev)..((by + bh) as usize * dev).min(h) {
                    for x in (bx as usize * dev)..((bx + bw) as usize * dev).min(w) {
                        img[(y * w + x) * 3..][..3].copy_from_slice(&c);
                    }
                }
            }
            for (x, y, line) in &scene.page {
                for (col, ch) in line.bytes().enumerate() {
                    let g = &super::super::font::FONT[(ch - 0x20) as usize];
                    for (gy, bits) in g.iter().enumerate() {
                        for gx in 0..8 {
                            if bits & (0x80 >> gx) == 0 {
                                continue;
                            }
                            for dy in 0..dev {
                                for dx in 0..dev {
                                    let px = (*x as usize + col * 8 + gx) * dev + dx;
                                    let py = (*y as usize + gy) * dev + dy;
                                    if px < w && py < h {
                                        img[(py * w + px) * 3..][..3].copy_from_slice(&scene.ink);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // `drawn` is front to back; paint it back to front, each
            // annotation's marks, then its glass, then its panel.
            for (panel, pieces) in drawn.iter().rev() {
                for p in pieces.iter().rev().filter(|p| !p.panel) {
                    blit(&mut img, w, h, dev, p.at, &p.raster);
                }
                if scene.glass {
                    glass(&mut img, w, h, dev, *panel);
                }
                for p in pieces.iter().filter(|p| p.panel) {
                    blit(&mut img, w, h, dev, p.at, &p.raster);
                }
            }
            if let Some(pieces) = &select {
                for (at, r) in pieces.iter().rev() {
                    blit(&mut img, w, h, dev, *at, r);
                }
            }
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            ppm.extend_from_slice(&img);
            std::fs::write(std::path::Path::new(dir).join(format!("{}.ppm", scene.name)), ppm).unwrap();
        }
    }

    /// A premultiplied pixel that is visibly the accent's hue.
    fn is_gold(p: &[u8]) -> bool {
        // Gold: red near full, green well up (not the danger red), blue low.
        let (r, g, b, a) = (p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32);
        a > 8.0 && r > 0.75 * a && g > 0.6 * r && b < 0.5 * r
    }
}
