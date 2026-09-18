// SPDX-License-Identifier: AGPL-3.0-only
//! The annotation pass (COMP-18, ADR 0040).
//!
//! A compositor-drawn pass above everything client-drawn and above the cursor,
//! and **below** trusted UI. It is not trusted UI and carries no phrase: the
//! text in it is written by an untrusted caller -- for Oracle-Eyes, by a model
//! summarising pixels an attacker chose -- and the whole point of ADR 0040 is
//! that such text must never share a position with the anti-spoof anchor.
//!
//! Two properties come from where it is drawn rather than from any check here:
//!
//! * **Capture-invisible.** [`super::capture::capture_elements`] builds its own
//!   pass list from the space and the layer map. Backend-prepended elements are
//!   not in it, so an annotation cannot appear in a screenshot, a screencast, or
//!   in the pixels Oracle-Eyes itself reads back.
//! * **Below trusted UI.** The per-frame element vector is front-to-back, so the
//!   backends splice annotations in *after* the indicator, never before.
//!
//! The caller supplies a rectangle and a string. Everything else -- clamping,
//! sanitising, styling, where the panel actually lands -- is decided here.

use smithay::{
    backend::allocator::Fourcc,
    backend::renderer::{
        element::{
            texture::{TextureBuffer, TextureRenderElement},
            Kind,
        },
        gles::{GlesRenderer, GlesTexture},
        ImportMem,
    },
    output::Output,
    utils::{Logical, Point, Rectangle, Scale, Size, Transform},
};

use super::{curve, text, AbyssRenderElement};

/// Text colour: eclipse amber, the same face the rest of the system uses.
const FG: text::Rgba = [1.0, 0.72, 0.20, 1.0];
/// Panel colour: near-black, translucent enough to read the content beneath.
const BG: text::Rgba = [0.02, 0.02, 0.03, 0.82];

/// Columns a panel wraps at. Fixed rather than derived from the anchor: a
/// caller must not be able to choose a width that shoulders other content off
/// the screen.
const COLS: usize = 30;

/// Gap between the anchor rectangle and the panel drawn beside it.
const GUTTER: i32 = 8;

/// The band drawn around the region an annotation is about, and the leader
/// that joins it to the panel. Deliberately the same amber as the region
/// selector: "this is the bit in question" should read identically whether
/// the compositor is asking or answering.
const BAND: text::Rgba = [1.0, 0.72, 0.20, 0.9];
/// Side of the square each corner bracket occupies, its corner radius and its
/// stroke weight, in logical pixels. Brackets, not a ring: four marks that say
/// "this much" while obscuring almost nothing of what they mark.
const MARK_SIDE: i32 = 20;
const MARK_RADIUS: i32 = 6;
const MARK_THICK: i32 = 2;
/// Weight of the line joining a marked region to its panel.
const LEAD_THICK: i32 = 2;

/// Glyph size, in device pixels per font pixel, on top of the output scale.
/// The 5x7 font at 1:1 is unreadable at arm's length; at 3x it is a 15x21
/// glyph, which is the smallest that reads comfortably on a 1x display.
const TEXT_PX: usize = 3;

/// Opaque handle to a live annotation. Handed back over the control socket;
/// the caller can address only annotations it created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnnotationId(pub u64);

/// One live annotation: the region it is about, and what to say about it.
#[derive(Debug, Clone)]
pub struct Annotation {
    /// Control-socket connection that created this annotation. A caller may
    /// address only its own handles, and everything it made goes away with it.
    pub owner: u64,
    /// Region of the output this annotation refers to, in logical coordinates.
    pub anchor: Rectangle<i32, Logical>,
    /// Already-sanitised, already-wrapped lines. Stored in final form so a
    /// caller's string is reduced once, at the door, rather than every frame.
    pub lines: Vec<String>,
}

/// Live annotations, owned by [`AbyssState`].
///
/// A plain map keyed by handle, per the handle-based-state invariant -- no
/// `Rc<RefCell<_>>`, nothing shared with another thread. Insertion order is not
/// meaningful; `next` only ever counts up, so a destroyed handle is never
/// reused and a stale reference fails closed.
#[derive(Debug, Default)]
pub struct AnnotationStore {
    live: std::collections::BTreeMap<AnnotationId, Annotation>,
    next: u64,
    /// The uploaded artwork for each live annotation, kept across frames.
    /// `TextureBuffer::from_texture` mints a fresh element id and a zeroed
    /// commit counter, so rebuilding it every frame would leave the damage
    /// tracker unable to match an element to last frame's and force a repaint
    /// of that region forever -- which is exactly the shimmer a displayed
    /// panel used to have. Rebuilt only when [`ArtKey`] changes.
    art: std::collections::BTreeMap<AnnotationId, Art>,
}

/// Everything that decides what an annotation looks like and where each piece
/// of it lands. Cheap to compute every frame; the artwork behind it is not.
type ArtKey = (
    Vec<String>,
    usize,
    usize,
    Rectangle<i32, Logical>,
    Size<i32, Logical>,
    Point<i32, Logical>,
);

/// One annotation's uploaded pieces, each with where it is drawn.
type Parts = Vec<(Point<i32, Logical>, TextureBuffer<GlesTexture>)>;

/// Where a leader's raster goes, how big it is, and its two endpoints within
/// it -- all logical.
type Leader = (
    Point<i32, Logical>,
    Size<i32, Logical>,
    Point<i32, Logical>,
    Point<i32, Logical>,
);

/// One annotation's uploaded pieces: the panel first, then its marker
/// brackets and leader, each with the logical position it is drawn at.
#[derive(Debug)]
struct Art {
    key: ArtKey,
    parts: Parts,
}

/// Furthest a caller's rectangle may sit from the origin, and the largest it
/// may be, in logical pixels. Well past any real output, but small enough that
/// every `loc + size` below stays inside `i32`: the anchor is now drawn, not
/// just used for placement, and an unprivileged socket caller must not be able
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

/// Hard cap on live annotations. A caller that leaks handles degrades its own
/// display, not the compositor's memory.
pub const MAX_LIVE: usize = 32;

impl AnnotationStore {
    /// Sanitise `text`, wrap it, and store it against a fresh handle owned by
    /// `owner`. Returns `None` once [`MAX_LIVE`] annotations are live.
    pub fn create(
        &mut self,
        owner: u64,
        anchor: Rectangle<i32, Logical>,
        text: &str,
    ) -> Option<AnnotationId> {
        if self.live.len() >= MAX_LIVE {
            return None;
        }
        self.next += 1;
        let id = AnnotationId(self.next);
        self.live.insert(
            id,
            Annotation {
                owner,
                anchor: clamp_anchor(anchor),
                lines: lines_for(text),
            },
        );
        Some(id)
    }

    /// Replace the text on a live handle (the expand flow, COMP-18 §3).
    /// Returns false for a handle that does not exist -- an unknown handle is
    /// not an error the caller gets to distinguish from a destroyed one.
    pub fn update(&mut self, owner: u64, id: AnnotationId, text: &str) -> bool {
        match self.live.get_mut(&id).filter(|a| a.owner == owner) {
            Some(a) => {
                a.lines = lines_for(text);
                true
            }
            None => false,
        }
    }

    /// Drop one handle. A handle owned by someone else is indistinguishable
    /// from one that never existed.
    pub fn destroy(&mut self, owner: u64, id: AnnotationId) -> bool {
        match self.live.get(&id) {
            Some(a) if a.owner == owner => self.live.remove(&id).is_some(),
            _ => false,
        }
    }

    /// Drop everything `owner` created. Also the disconnect path: a caller
    /// that goes away leaves nothing on screen.
    pub fn clear_for(&mut self, owner: u64) -> usize {
        let before = self.live.len();
        self.live.retain(|_, a| a.owner != owner);
        before - self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&AnnotationId, &Annotation)> {
        self.live.iter()
    }
}

/// The full text reduction, in one place so `create` and `update` cannot drift.
fn lines_for(s: &str) -> Vec<String> {
    text::wrap(&text::sanitize(s), COLS)
}

/// Build this output's annotation elements.
///
/// Returns front-to-back, like every other element list. The caller splices
/// these in below trusted UI and above the cursor.
/// `output_loc` is the output's origin in the global space; anchors arrive in
/// global coordinates and are drawn output-local.
pub fn annotation_elements(
    renderer: &mut GlesRenderer,
    store: &mut AnnotationStore,
    output: &Output,
    output_loc: Point<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    if store.is_empty() {
        return Vec::new();
    }
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let fractional = output.current_scale().fractional_scale();
    let scale = Scale::from(fractional);
    let logical: Size<i32, Logical> = mode.size.to_f64().to_logical(scale).to_i32_round();
    // Integer multiplier: the font is unhinted and unantialiased, so a
    // fractional scale would only smear it. The curves are drawn at `dev`,
    // the plain device-pixel ratio, where they do get antialiasing.
    let dev = (fractional.round() as usize).max(1);
    let px = dev * TEXT_PX;

    // Split borrow: the live map is read while its art cache is written.
    let AnnotationStore { live, art, .. } = store;
    art.retain(|id, _| live.contains_key(id));

    let mut elements = Vec::new();
    for (id, annotation) in live.iter() {
        if annotation.lines.is_empty() {
            continue;
        }
        let key: ArtKey = (
            annotation.lines.clone(),
            px,
            dev,
            annotation.anchor,
            logical,
            output_loc,
        );
        if art.get(id).map(|a| a.key != key).unwrap_or(true) {
            // A failed upload drops this one annotation rather than the frame.
            match build(
                renderer,
                annotation.anchor,
                &annotation.lines,
                px,
                dev,
                logical,
                output_loc,
            ) {
                Some(parts) => {
                    art.insert(*id, Art { key, parts });
                }
                None => {
                    art.remove(id);
                    continue;
                }
            }
        }
        let Some(a) = art.get(id) else { continue };
        for (loc, buffer) in &a.parts {
            elements.push(AbyssRenderElement::Texture(
                TextureRenderElement::from_texture_buffer(
                    loc.to_f64().to_physical(scale),
                    buffer,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                ),
            ));
        }
    }
    elements
}

/// Rasterise and upload one annotation: the panel, then -- when there is an
/// honest region to point at -- four corner brackets and the line joining
/// them to the panel. `None` if any upload fails.
///
/// The panel alone says *what*; the brackets and the leader say *about what*.
/// All of it is in the same pass, so it inherits capture-invisibility and its
/// place below trusted UI without anything new having to be true.
fn build(
    renderer: &mut GlesRenderer,
    anchor: Rectangle<i32, Logical>,
    lines: &[String],
    px: usize,
    dev: usize,
    logical: Size<i32, Logical>,
    output_loc: Point<i32, Logical>,
) -> Option<Parts> {
    let raster = text::rasterize(lines, px, FG, BG);
    // The raster is already in physical pixels; its logical size is what
    // placement works in.
    let size: Size<i32, Logical> = (raster.w / px as i32, raster.h / px as i32).into();
    let loc = place(anchor, size, logical, output_loc);
    let mut parts = vec![(loc, upload(renderer, &raster, px)?)];

    let region = drawable_region(Rectangle::new(anchor.loc - output_loc, anchor.size), logical);
    // Nothing honest to point at: draw the glyphs and no marker.
    let Some(region) = region else { return Some(parts) };

    let side = MARK_SIDE.min(region.size.w).min(region.size.h).max(1);
    let mark = bracket(side, dev);
    for (at, fx, fy) in slots(region, side) {
        let flipped = text::Raster {
            w: mark.w,
            h: mark.h,
            px: flip(&mark.px, mark.w, mark.h, fx, fy),
        };
        parts.push((at, upload(renderer, &flipped, dev)?));
    }
    if let Some((at, span, a, b)) = leader(region, Rectangle::new(loc, size)) {
        parts.push((at, upload(renderer, &line(span, a, b, dev), dev)?));
    }
    Some(parts)
}

/// A top-left corner bracket: a quarter-arc with a tangent arm along each
/// edge, drawn into a `side`-square raster at `dev` device pixels per logical
/// one. The other three corners are this one flipped, so the curve is
/// rasterised once per annotation rather than four times.
fn bracket(side: i32, dev: usize) -> text::Raster {
    use std::f32::consts::{FRAC_PI_2, PI};
    let n = (side.max(1) as usize) * dev;
    let mut px = vec![0u8; n * n * 4];
    let thick = (MARK_THICK as usize * dev) as f32;
    let inset = thick * 0.5;
    let radius = ((MARK_RADIUS as usize * dev) as f32).min(n as f32 - thick);
    let c = inset + radius;
    curve::stroke_arc(&mut px, n, n, c, c, radius, thick, PI, PI + FRAC_PI_2, BAND);
    curve::stroke_line(&mut px, n, n, inset, c, inset, n as f32, thick, BAND);
    curve::stroke_line(&mut px, n, n, c, inset, n as f32, inset, thick, BAND);
    text::Raster {
        w: n as i32,
        h: n as i32,
        px,
    }
}

/// Mirror a raster in either axis. Premultiplied RGBA moves whole, so this is
/// a pixel shuffle and nothing more.
fn flip(src: &[u8], w: i32, h: i32, fx: bool, fy: bool) -> Vec<u8> {
    let (w, h) = (w.max(0) as usize, h.max(0) as usize);
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let sy = if fy { h - 1 - y } else { y };
        for x in 0..w {
            let sx = if fx { w - 1 - x } else { x };
            let (d, s) = ((y * w + x) * 4, (sy * w + sx) * 4);
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    out
}

/// Where each corner bracket sits, and which axes its artwork is mirrored in:
/// top-left, top-right, bottom-right, bottom-left.
fn slots(r: Rectangle<i32, Logical>, side: i32) -> [(Point<i32, Logical>, bool, bool); 4] {
    let (x0, y0) = (r.loc.x, r.loc.y);
    let (x1, y1) = (r.loc.x + r.size.w - side, r.loc.y + r.size.h - side);
    [
        (Point::from((x0, y0)), false, false),
        (Point::from((x1, y0)), true, false),
        (Point::from((x1, y1)), true, true),
        (Point::from((x0, y1)), false, true),
    ]
}

/// The line joining a marked region to its panel: where its raster goes, how
/// big it is, and its two endpoints within it, all in logical pixels.
///
/// Each end is the point of one rectangle nearest the other's centre, so the
/// line leaves the region on the side the panel is actually on. `None` when
/// the two are close enough to touch -- there is nothing to join.
fn leader(r: Rectangle<i32, Logical>, panel: Rectangle<i32, Logical>) -> Option<Leader> {
    if r.size.w <= 0 || r.size.h <= 0 || panel.size.w <= 0 || panel.size.h <= 0 {
        return None;
    }
    let near = |b: Rectangle<i32, Logical>, to: Point<i32, Logical>| {
        Point::from((
            to.x.clamp(b.loc.x, b.loc.x + b.size.w),
            to.y.clamp(b.loc.y, b.loc.y + b.size.h),
        ))
    };
    let centre = |b: Rectangle<i32, Logical>| Point::from((b.loc.x + b.size.w / 2, b.loc.y + b.size.h / 2));
    let a = near(r, centre(panel));
    let b = near(panel, centre(r));
    if (a.x - b.x).abs() < LEAD_THICK && (a.y - b.y).abs() < LEAD_THICK {
        return None;
    }
    let pad = LEAD_THICK;
    let at = Point::from((a.x.min(b.x) - pad, a.y.min(b.y) - pad));
    let span = Size::from(((a.x - b.x).abs() + pad * 2, (a.y - b.y).abs() + pad * 2));
    Some((at, span, a - at, b - at))
}

/// The leader's raster: one antialiased straight run, no elbow.
fn line(
    span: Size<i32, Logical>,
    a: Point<i32, Logical>,
    b: Point<i32, Logical>,
    dev: usize,
) -> text::Raster {
    let (w, h) = ((span.w.max(1) as usize) * dev, (span.h.max(1) as usize) * dev);
    let mut px = vec![0u8; w * h * 4];
    let d = dev as f32;
    curve::stroke_line(
        &mut px,
        w,
        h,
        a.x as f32 * d,
        a.y as f32 * d,
        b.x as f32 * d,
        b.y as f32 * d,
        (LEAD_THICK as usize * dev) as f32,
        BAND,
    );
    text::Raster {
        w: w as i32,
        h: h as i32,
        px,
    }
}

/// What of a caller's region may actually be drawn.
///
/// `place` only ever used the anchor to position a panel it then clamped, so a
/// silly rectangle cost a caller nothing but a badly-placed panel. The band and
/// the leader *draw* it, and `annotation_create` takes any `i32` position and
/// any positive size -- so without this a control-socket caller could outline
/// the whole screen, or ring another client's UI, which is on-screen geometry
/// of its choosing rather than glyphs. Capping the size at a third of the
/// output per axis and clipping to the output leaves every real region
/// untouched while taking that choice away. Returns `None` when nothing is
/// left, in which case the panel is drawn unmarked.
fn drawable_region(
    r: Rectangle<i32, Logical>,
    output: Size<i32, Logical>,
) -> Option<Rectangle<i32, Logical>> {
    if r.size.w <= 0 || r.size.h <= 0 {
        return None;
    }
    // Clip first, then cap: the cap is on what is drawn, so a huge rectangle
    // reaching onto the output keeps its visible corner rather than being
    // trimmed off-screen and vanishing.
    let on = r.intersection(Rectangle::new(Point::from((0, 0)), output))?;
    Some(Rectangle::new(
        on.loc,
        Size::from((
            on.size.w.min((output.w / 3).max(1)),
            on.size.h.min((output.h / 3).max(1)),
        )),
    ))
}

/// Put the panel just below its anchor, nudged back on screen if it would fall
/// off, and flipped above the anchor if there is no room beneath it.
fn place(
    anchor: Rectangle<i32, Logical>,
    size: Size<i32, Logical>,
    output: Size<i32, Logical>,
    output_loc: Point<i32, Logical>,
) -> Point<i32, Logical> {
    let local = anchor.loc - output_loc;
    let mut y = local.y + anchor.size.h + GUTTER;
    if y + size.h > output.h {
        let above = local.y - size.h - GUTTER;
        y = if above >= 0 {
            above
        } else {
            (output.h - size.h).max(0)
        };
    }
    let x = local.x.min(output.w - size.w).max(0);
    (x, y).into()
}

/// Upload a raster to a texture, ready to be drawn at buffer scale `bs`.
///
/// `None` on any GL failure: an annotation is cosmetic and must never be able
/// to take a frame down with it.
fn upload(
    renderer: &mut GlesRenderer,
    raster: &text::Raster,
    bs: usize,
) -> Option<TextureBuffer<GlesTexture>> {
    // `Abgr8888` is RGBA in memory order on a little-endian host, which is how
    // `text::rasterize` lays the panel out.
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

    #[test]
    fn handles_are_never_reused() {
        let mut s = AnnotationStore::default();
        let a = s.create(1, rect(0, 0, 10, 10), "one").unwrap();
        assert!(s.destroy(1, a));
        let b = s.create(1, rect(0, 0, 10, 10), "two").unwrap();
        assert_ne!(a, b, "a destroyed handle must not come back");
        assert!(!s.destroy(1, a), "the stale handle must fail closed");
    }

    #[test]
    fn text_is_reduced_at_the_door() {
        // Whatever the caller sends, what is stored is already sanitised --
        // there is no path that renders the raw string.
        let mut s = AnnotationStore::default();
        let id = s.create(1, rect(0, 0, 10, 10), "a\u{1b}[2Jb\u{0}c").unwrap();
        let stored = s.iter().next().unwrap().1.lines.join("\n");
        assert_eq!(stored, "a[2Jbc");
        assert!(s.update(1, id, "\u{7}x"));
        assert_eq!(s.iter().next().unwrap().1.lines.join("\n"), "x");
    }

    #[test]
    fn update_on_an_unknown_handle_is_false_not_a_new_annotation() {
        let mut s = AnnotationStore::default();
        assert!(!s.update(1, AnnotationId(42), "hi"));
        assert!(s.is_empty());
    }

    #[test]
    fn a_caller_can_only_address_its_own_handles() {
        let mut s = AnnotationStore::default();
        let mine = s.create(1, rect(0, 0, 10, 10), "mine").unwrap();
        let theirs = s.create(2, rect(0, 0, 10, 10), "theirs").unwrap();
        assert!(!s.update(2, mine, "hijack"));
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
            assert!(s.create(1, rect(0, 0, 10, 10), "x").is_some());
        }
        assert!(s.create(1, rect(0, 0, 10, 10), "x").is_none());
        assert_eq!(s.clear_for(1), MAX_LIVE);
        assert!(s.create(1, rect(0, 0, 10, 10), "x").is_some());
    }

    /// ADR 0040: annotations are capture-invisible *by construction* --
    /// [`super::super::capture::capture_elements`] builds its own pass list
    /// and never consults this module. There is no GL context in a unit test
    /// to compare two rendered lists, so the assertion is made where the
    /// property actually lives: in the source of the capture pass.
    #[test]
    fn the_capture_pass_cannot_see_annotations() {
        let src = include_str!("capture.rs");
        assert!(
            !src.contains("annotation"),
            "capture.rs referenced the annotation pass; capture exclusion is \
             supposed to hold because it never looks"
        );
    }

    /// COMP-18 §1: the per-frame element vector is front-to-back, so trusted
    /// UI must be spliced ahead of annotations in every backend. Each backend
    /// expresses that differently -- drm appends, winit and headless splice at
    /// zero -- so the check is per file and on the relative order of the two
    /// calls, which is the thing that must not be swapped.
    #[test]
    fn the_trusted_indicator_stays_above_annotations_in_every_backend() {
        for (src, path, indicator_first) in [
            (include_str!("../backend/drm.rs"), "drm.rs", true),
            (include_str!("../backend/winit.rs"), "winit.rs", false),
            (include_str!("../backend/headless.rs"), "headless.rs", false),
        ] {
            let ann = src
                .find("annotation::annotation_elements")
                .unwrap_or_else(|| panic!("{path} does not draw annotations"));
            let ind = src
                .find("capture::indicator")
                .unwrap_or_else(|| panic!("{path} does not draw the indicator"));
            assert_eq!(
                ind < ann,
                indicator_first,
                "{path} puts the annotation pass on the wrong side of trusted UI"
            );
        }
    }

    #[test]
    fn the_brackets_never_overlap_on_a_tiny_region() {
        let r = rect(10, 10, 3, 40);
        let side = MARK_SIDE.min(r.size.w).min(r.size.h).max(1);
        assert_eq!(side, 3);
        let s = slots(r, side);
        // Four distinct corners, each wholly inside the region.
        for (p, _, _) in s {
            assert!(p.x >= r.loc.x && p.x + side <= r.loc.x + r.size.w);
            assert!(p.y >= r.loc.y && p.y + side <= r.loc.y + r.size.h);
        }
        assert_eq!(s[0].0, Point::from((10, 10)));
        assert_eq!(s[2].0, Point::from((10, 47)));
    }

    #[test]
    fn the_leader_only_exists_when_there_is_a_gap_to_cross() {
        // Panel well below the region: a line, inside a box spanning the gap.
        let (at, span, a, b) = leader(rect(100, 100, 40, 40), rect(100, 300, 200, 60)).unwrap();
        assert!(span.w >= LEAD_THICK * 2 && span.h >= LEAD_THICK * 2);
        assert_eq!(at + a, Point::from((140, 140)));
        assert_eq!(at + b, Point::from((120, 300)));
        // Touching: nothing to join.
        assert!(leader(rect(0, 0, 40, 40), rect(0, 40, 40, 10)).is_none());
        // Degenerate input is refused rather than drawn.
        assert!(leader(rect(0, 0, 0, 0), rect(0, 300, 10, 10)).is_none());
    }

    #[test]
    fn a_marker_cannot_be_made_to_frame_the_screen() {
        let out: Size<i32, Logical> = (1920, 1080).into();
        // A caller asking to outline everything gets a third of it, on-screen.
        let r = drawable_region(rect(-500, -500, 9000, 9000), out).unwrap();
        assert_eq!((r.loc.x, r.loc.y), (0, 0));
        assert_eq!((r.size.w, r.size.h), (640, 360));
        // A real OCR region is untouched.
        let r = drawable_region(rect(300, 200, 400, 60), out).unwrap();
        assert_eq!((r.loc.x, r.loc.y, r.size.w, r.size.h), (300, 200, 400, 60));
        // Entirely off this output: nothing to draw.
        assert!(drawable_region(rect(5000, 5000, 100, 100), out).is_none());
    }

    #[test]
    fn an_absurd_anchor_is_reduced_at_the_door() {
        // The anchor is drawn now, not just used for placement, so a socket
        // caller must not be able to overflow the additions in `band`,
        // `leader` and `place`.
        let mut store = AnnotationStore::default();
        let id = store
            .create(
                1,
                Rectangle::new(
                    Point::from((i32::MAX, i32::MIN)),
                    Size::from((i32::MAX, i32::MAX)),
                ),
                "hi",
            )
            .unwrap();
        let a = &store.live[&id];
        assert_eq!(a.anchor.loc.x, MAX_COORD);
        assert_eq!(a.anchor.loc.y, -MAX_COORD);
        assert_eq!(a.anchor.size.w, MAX_COORD);
        assert!(a.anchor.loc.x.checked_add(a.anchor.size.w).is_some());
    }

    #[test]
    fn placement_stays_on_the_output() {
        let out: Size<i32, Logical> = (1920, 1080).into();
        let size: Size<i32, Logical> = (300, 100).into();
        let origin = Point::from((0, 0));

        // Normal case: directly below the anchor.
        assert_eq!(
            place(rect(100, 100, 50, 20), size, out, origin),
            (100, 128).into()
        );
        // No room below: flips above.
        assert_eq!(
            place(rect(100, 1000, 50, 20), size, out, origin),
            (100, 892).into()
        );
        // Would run off the right edge: pulled back.
        assert_eq!(place(rect(1900, 100, 10, 20), size, out, origin).x, 1620);
        // Output-relative: a second output's annotation is placed in its own space.
        assert_eq!(place(rect(2020, 100, 50, 20), size, out, (1920, 0).into()).x, 100);
    }

    #[test]
    fn a_panel_taller_than_the_output_is_clamped_not_negative() {
        let out: Size<i32, Logical> = (800, 60).into();
        let size: Size<i32, Logical> = (200, 400).into();
        let p = place(rect(0, 0, 10, 10), size, out, (0, 0).into());
        assert_eq!(p, (0, 0).into());
    }
}
