// SPDX-License-Identifier: AGPL-3.0-only
//! The annotation pass (COMP-18, ADR 0040, ADR 0054).
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
//!   in the pixels Oracle-Eyes itself reads back. The pick marker is part of
//!   the same pass and inherits this unchanged.
//! * **Below trusted UI.** The per-frame element vector is front-to-back, so the
//!   backends splice annotations in *after* the indicator, never before.
//!
//! The caller supplies a rectangle, a title, a string and optionally a pick.
//! Everything else -- clamping, sanitising, styling, the panel's width and
//! where it actually lands -- is decided here.

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

use super::{text, AbyssRenderElement};

/// Columns a panel wraps at: chosen per output from this range, never by the
/// caller, so a caller cannot pick a width that shoulders other content off
/// the screen. See [`columns`].
const MIN_COLS: usize = 28;
const MAX_COLS: usize = 48;

/// Longest title kept, in characters, before wrapping. ADR 0054: a headline,
/// not a second body.
const MAX_TITLE: usize = 80;

/// Gap between the region an annotation is about and the panel beside it.
const GUTTER: i32 = 14;

/// The brackets drawn at the corners of the region an annotation is about,
/// and the leader that joins it to the panel. Deliberately the same amber as
/// the region selector: "this is the bit in question" should read identically
/// whether the compositor is asking or answering. Square, like the panel.
const BAND: text::Rgba = [1.0, 0.72, 0.20, 0.9];
const MARK_SIDE: i32 = 14;
const MARK_THICK: i32 = 2;
/// Weight of the leader, and the side of the square that ends it at the region.
const LEAD_THICK: i32 = 1;
const LEAD_END: i32 = 5;

/// The pick marker (ADR 0054): a wash over the chosen option, a bar down its
/// left edge and, left of that, the same inverted chip the panel's header
/// carries. The wash is faint -- it must not stop the option being read.
const PICK_WASH: text::Rgba = [1.0, 0.72, 0.20, 0.16];
const PICK_BAR: i32 = 3;
/// Gap between the bar and the option's text, and between the tab and the bar.
const PICK_GAP: i32 = 4;

/// Opaque handle to a live annotation. Handed back over the control socket;
/// the caller can address only annotations it created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnnotationId(pub u64);

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
    pub anchor: Rectangle<i32, Logical>,
    /// Already-sanitised title and body. Stored reduced so a caller's string
    /// is dealt with once, at the door; wrapping waits for the output, whose
    /// width decides the column count.
    pub title: String,
    pub text: String,
    pub pick: Option<Pick>,
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
/// of it lands: the card, the device scale, the drawn region and pick, and
/// the panel's position. Cheap to compute every frame; the artwork behind it
/// is not.
type ArtKey = (
    text::Card,
    usize,
    Option<Rectangle<i32, Logical>>,
    Option<Rectangle<i32, Logical>>,
    Point<i32, Logical>,
);

/// One annotation's rasters, each with where it is drawn, output-local and
/// logical. All of them are at the output's integer device scale.
type Drawn = Vec<(Point<i32, Logical>, text::Raster)>;

/// One annotation's uploaded pieces, front to back: the panel, then the pick
/// marker, the brackets and the leader.
#[derive(Debug)]
struct Art {
    key: ArtKey,
    parts: Vec<(Point<i32, Logical>, TextureBuffer<GlesTexture>)>,
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

/// A pick label as the compositor will draw it: one or two ASCII letters or
/// digits, upper-cased. Anything else is `None` -- the label is drawn in a
/// chip beside the caller's region and is the one piece of caller text that
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
    /// annotation is kept: the text is still worth showing.
    pub fn create(
        &mut self,
        owner: u64,
        anchor: Rectangle<i32, Logical>,
        title: &str,
        text: &str,
        pick: Option<Pick>,
    ) -> Option<AnnotationId> {
        if self.live.len() >= MAX_LIVE {
            return None;
        }
        self.next += 1;
        let id = AnnotationId(self.next);
        let anchor = clamp_anchor(anchor);
        self.live.insert(
            id,
            Annotation {
                owner,
                anchor,
                title: title_for(title),
                text: text::sanitize(text),
                pick: admit(anchor, pick),
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
                a.title = title_for(title);
                a.text = text::sanitize(text);
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

/// A title is one line of text, capped hard.
fn title_for(s: &str) -> String {
    text::sanitize_line(s).chars().take(MAX_TITLE).collect()
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
    // fractional scale would only smear it.
    let dev = (fractional.round() as usize).max(1);

    // Split borrow: the live map is read while its art cache is written.
    let AnnotationStore { live, art, .. } = store;
    art.retain(|id, _| live.contains_key(id));

    let mut elements = Vec::new();
    // Panels already placed on this output this frame, so the next one can
    // keep clear of them (COMP-18 §3, collision avoidance).
    let mut taken = Vec::new();
    for (id, annotation) in live.iter() {
        let Some(layout) = lay_out(annotation, logical, output_loc, &taken) else {
            continue;
        };
        taken.push(layout.panel);
        let key: ArtKey = (
            layout.card.clone(),
            dev,
            layout.region,
            layout.pick.as_ref().map(|p| p.rect),
            layout.panel.loc,
        );
        if art.get(id).map(|a| a.key != key).unwrap_or(true) {
            // A failed upload drops this one annotation rather than the frame.
            let parts: Option<Vec<_>> = draw(&layout, dev)
                .into_iter()
                .map(|(at, r)| upload(renderer, &r, dev).map(|b| (at, b)))
                .collect();
            match parts {
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

/// Where everything about one annotation goes on one output, output-local and
/// logical. Pure: computed every frame, and the tests draw from it directly.
#[derive(Debug, Clone)]
struct Layout {
    card: text::Card,
    panel: Rectangle<i32, Logical>,
    /// The part of the anchor that may be marked, `None` when there is no
    /// honest region to point at -- the panel is then drawn unmarked.
    region: Option<Rectangle<i32, Logical>>,
    pick: Option<Pick>,
}

fn lay_out(
    a: &Annotation,
    output: Size<i32, Logical>,
    output_loc: Point<i32, Logical>,
    taken: &[Rectangle<i32, Logical>],
) -> Option<Layout> {
    let local = |r: Rectangle<i32, Logical>| Rectangle::new(r.loc - output_loc, r.size);
    let region = drawable_region(local(a.anchor), output, a.pick.as_ref().map(|p| local(p.rect)));
    // A pick is drawn only inside what is drawn of the anchor: if it is
    // taller or wider than the cap, it goes unmarked rather than half-marked.
    let pick = a.pick.as_ref().and_then(|p| {
        let rect = local(p.rect);
        region.filter(|r| r.contains_rect(rect)).map(|_| Pick {
            rect,
            label: p.label.clone(),
        })
    });
    let chip = pick.as_ref().map(|p| p.label.as_str()).unwrap_or("");
    let cols = columns(chip, &a.title, &a.text, output.w);
    let card = text::Card::new(chip, &a.title, &a.text, cols);
    if card.is_empty() {
        return None;
    }
    let (w, h) = card.size();
    let size = Size::from((w as i32, h as i32));
    // What the panel must not cover: the region, and the tab that sticks out
    // to the left of a pick.
    let target = region.unwrap_or_else(|| local(a.anchor));
    let blocked = match &pick {
        Some(p) => target.merge(tab_rect(p)),
        None => target,
    };
    let focus = pick.as_ref().map(|p| p.rect).unwrap_or(target);
    let loc = place(blocked, focus, pick.is_some(), size, output, taken);
    Some(Layout {
        card,
        panel: Rectangle::new(loc, size),
        region,
        pick,
    })
}

/// The column count for a panel on an output `output_w` logical pixels wide.
///
/// The widest the output allows -- a third of it, within [`MIN_COLS`] and
/// [`MAX_COLS`] -- and then narrowed while that costs no extra line, so a
/// three-line answer is three even lines rather than two full ones and a
/// straggler.
fn columns(chip: &str, title: &str, body: &str, output_w: i32) -> usize {
    let room = (output_w.max(0) as usize / 3).saturating_sub(text::CHROME_W) / super::font::ADVANCE;
    let widest = room.clamp(MIN_COLS, MAX_COLS);
    let rows = |cols: usize| {
        let c = text::Card::new(chip, title, body, cols);
        (c.title.len(), c.body.len())
    };
    let want = rows(widest);
    let mut cols = widest;
    while cols > MIN_COLS && rows(cols - 1) == want {
        cols -= 1;
    }
    cols
}

/// Draw one annotation, front to back: the panel, then -- when there is an
/// honest region to point at -- the pick marker, the corner brackets and the
/// leader. The panel alone says *what*; the rest says *about what*. All of it
/// is in the same pass, so it inherits capture-invisibility and its place
/// below trusted UI without anything new having to be true.
fn draw(l: &Layout, dev: usize) -> Drawn {
    let mut out = vec![(l.panel.loc, text::rasterize(&l.card, dev))];
    let Some(region) = l.region else { return out };
    if let Some(p) = &l.pick {
        let t = tab_rect(p);
        out.push((t.loc, text::tab(&p.label, dev)));
        let bar = bar_rect(p);
        out.push((bar.loc, paint(bar.size, dev, |_, _| Some(text::AMBER))));
        let wash = Rectangle::new(p.rect.loc - Point::from((2, 2)), p.rect.size + Size::from((4, 4)));
        out.push((wash.loc, paint(wash.size, dev, |_, _| Some(PICK_WASH))));
    }
    let side = MARK_SIDE.min(region.size.w).min(region.size.h).max(1);
    for (at, fx, fy) in slots(region, side) {
        let t = MARK_THICK.min(side);
        out.push((
            at,
            paint(Size::from((side, side)), dev, |x, y| {
                let (x, y) = (
                    if fx { side - 1 - x } else { x },
                    if fy { side - 1 - y } else { y },
                );
                (x < t || y < t).then_some(BAND)
            }),
        ));
    }
    let from = match &l.pick {
        Some(p) => region.merge(tab_rect(p)),
        None => region,
    };
    let focus = l.pick.as_ref().map(|p| p.rect).unwrap_or(region);
    if let Some(path) = leader(from, focus, l.panel) {
        out.push(leader_raster(&path, dev));
    }
    out
}

/// The chip beside a pick: left of the bar, level with the option's first
/// line. It may stick out of the anchor to the left; that is compositor
/// geometry keyed to a rectangle already inside it, not the caller's.
fn tab_rect(p: &Pick) -> Rectangle<i32, Logical> {
    let (w, h) = text::tab_size(&p.label);
    let (w, h) = (w as i32, h as i32);
    let first = p.rect.size.h.min(super::font::LINE_H as i32);
    let x = bar_rect(p).loc.x - PICK_GAP - w;
    let y = p.rect.loc.y + (first - h) / 2;
    Rectangle::new((x, y).into(), (w, h).into())
}

fn bar_rect(p: &Pick) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (p.rect.loc.x - PICK_GAP - PICK_BAR, p.rect.loc.y - 2).into(),
        (PICK_BAR, p.rect.size.h + 4).into(),
    )
}

/// A raster of `size` logical pixels whose every pixel is `f(x, y)`, scaled
/// to `dev`. Everything this pass draws besides the card is a handful of
/// axis-aligned rectangles, so this is the only brush it needs.
fn paint(size: Size<i32, Logical>, dev: usize, f: impl Fn(i32, i32) -> Option<text::Rgba>) -> text::Raster {
    let (w, h) = (size.w.max(1), size.h.max(1));
    let (dw, dh) = (w as usize * dev, h as usize * dev);
    let mut px = vec![0u8; dw * dh * 4];
    for y in 0..h {
        for x in 0..w {
            let Some(c) = f(x, y) else { continue };
            let c = text::premul(c);
            for dy in 0..dev {
                let row = (y as usize * dev + dy) * dw;
                for dx in 0..dev {
                    let i = (row + x as usize * dev + dx) * 4;
                    px[i..i + 4].copy_from_slice(&c);
                }
            }
        }
    }
    text::Raster {
        w: dw as i32,
        h: dh as i32,
        px,
    }
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

/// The leader from a marked region to its panel, as the corners of a path of
/// axis-aligned runs: straight across when the two share a row or a column,
/// otherwise out, along, and in again. It leaves `from` level with `focus`
/// -- the pick, when there is one -- so it points at the answer, not at the
/// middle of the question. `None` when there is no gap to cross.
fn leader(
    from: Rectangle<i32, Logical>,
    focus: Rectangle<i32, Logical>,
    panel: Rectangle<i32, Logical>,
) -> Option<Vec<Point<i32, Logical>>> {
    if from.size.w <= 0 || from.size.h <= 0 || panel.size.w <= 0 || panel.size.h <= 0 {
        return None;
    }
    let (fr, fb) = (from.loc.x + from.size.w, from.loc.y + from.size.h);
    let (pr, pb) = (panel.loc.x + panel.size.w, panel.loc.y + panel.size.h);
    let inset = |lo: i32, hi: i32, v: i32| v.clamp(lo + LEAD_END, (hi - LEAD_END).max(lo + LEAD_END));
    let mid = |a: i32, b: i32| a + (b - a) / 2;
    let beside = panel.loc.x >= fr || pr <= from.loc.x;
    let stacked = panel.loc.y >= fb || pb <= from.loc.y;
    let path = if beside && !(stacked && panel.loc.x < fr + GUTTER && pr > from.loc.x - GUTTER) {
        let y0 = inset(
            from.loc.y,
            fb,
            focus.loc.y + focus.size.h.min(super::font::LINE_H as i32) / 2,
        );
        let y1 = inset(panel.loc.y, pb, y0);
        let (x0, x1) = if panel.loc.x >= fr {
            (fr, panel.loc.x)
        } else {
            (from.loc.x, pr)
        };
        if (x1 - x0).abs() < LEAD_THICK * 2 {
            return None;
        }
        let xm = mid(x0, x1);
        vec![(x0, y0), (xm, y0), (xm, y1), (x1, y1)]
    } else if stacked {
        let x0 = inset(from.loc.x, fr, focus.loc.x + focus.size.w / 2);
        let x1 = inset(panel.loc.x, pr, x0);
        let (y0, y1) = if panel.loc.y >= fb {
            (fb, panel.loc.y)
        } else {
            (from.loc.y, pb)
        };
        if (y1 - y0).abs() < LEAD_THICK * 2 {
            return None;
        }
        let ym = mid(y0, y1);
        vec![(x0, y0), (x0, ym), (x1, ym), (x1, y1)]
    } else {
        return None;
    };
    Some(path.into_iter().map(Point::from).collect())
}

/// Rasterise a leader path: each run `LEAD_THICK` wide, and a square
/// terminal where it meets the region.
fn leader_raster(path: &[Point<i32, Logical>], dev: usize) -> (Point<i32, Logical>, text::Raster) {
    let pad = LEAD_END;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for p in path {
        (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
    }
    let at = Point::from((x0 - pad, y0 - pad));
    let size = Size::from((x1 - x0 + pad * 2 + 1, y1 - y0 + pad * 2 + 1));
    let runs: Vec<_> = path
        .windows(2)
        .map(|w| {
            let (a, b) = (w[0] - at, w[1] - at);
            (
                a.x.min(b.x),
                a.y.min(b.y),
                a.x.max(b.x) + LEAD_THICK - 1,
                a.y.max(b.y) + LEAD_THICK - 1,
            )
        })
        .collect();
    let end = path[0] - at;
    let half = LEAD_END / 2;
    let raster = paint(size, dev, |x, y| {
        let on_run = runs
            .iter()
            .any(|&(l, t, r, b)| (l..=r).contains(&x) && (t..=b).contains(&y));
        let on_end = (x - end.x).abs() <= half && (y - end.y).abs() <= half;
        (on_run || on_end).then_some(BAND)
    });
    (at, raster)
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
///
/// A capped region keeps its top-left, unless that would cut off `keep` --
/// the pick, already known to lie inside `r` -- in which case the window
/// slides along `r` just far enough to hold it. Where it sits within `r` is
/// the compositor's choice either way; the size cap is unchanged.
fn drawable_region(
    r: Rectangle<i32, Logical>,
    output: Size<i32, Logical>,
    keep: Option<Rectangle<i32, Logical>>,
) -> Option<Rectangle<i32, Logical>> {
    if r.size.w <= 0 || r.size.h <= 0 {
        return None;
    }
    // Clip first, then cap: the cap is on what is drawn, so a huge rectangle
    // reaching onto the output keeps its visible corner rather than being
    // trimmed off-screen and vanishing.
    let on = r.intersection(Rectangle::new(Point::from((0, 0)), output))?;
    let (w, h) = (
        on.size.w.min((output.w / 3).max(1)),
        on.size.h.min((output.h / 3).max(1)),
    );
    // The start of a `len`-long window within `lo..lo + span` that covers
    // `k0..k1` if it can, and otherwise at least its far end.
    let slide = |lo: i32, span: i32, len: i32, k0: i32, k1: i32| {
        (k1 - len).max(lo).min(k0).min(lo + span - len).max(lo)
    };
    let loc = match keep {
        Some(k) => Point::from((
            slide(on.loc.x, on.size.w, w, k.loc.x, k.loc.x + k.size.w),
            slide(on.loc.y, on.size.h, h, k.loc.y, k.loc.y + k.size.h),
        )),
        None => on.loc,
    };
    Some(Rectangle::new(loc, Size::from((w, h))))
}

/// Where a panel of `size` goes, beside `blocked` and as near `focus` as it
/// can get.
///
/// Four candidates -- right of the region, below, left, above -- each slid
/// along its edge to line up with `focus` and then kept on the output. A
/// candidate counts if it fits on the output without covering the region or a
/// panel already placed. Of those, the one whose near edge is closest to the
/// focus's centre *across the gap* wins, ties going in that order: that is
/// half the focus's width for a side panel and half its height for a stacked
/// one, so a wide paragraph gets its answer underneath and a narrow column
/// gets it alongside. With a pick (`level`) a side panel wins whenever one
/// fits, level with the option, so the leader runs straight at it. If nothing
/// fits clear of the other panels, overlap with them is allowed; if nothing
/// fits at all, the panel is clamped onto the output below the region, which
/// is where it always used to go.
fn place(
    blocked: Rectangle<i32, Logical>,
    focus: Rectangle<i32, Logical>,
    level: bool,
    size: Size<i32, Logical>,
    output: Size<i32, Logical>,
    taken: &[Rectangle<i32, Logical>],
) -> Point<i32, Logical> {
    let (w, h) = (size.w, size.h);
    let (bx, by, br, bb) = (
        blocked.loc.x,
        blocked.loc.y,
        blocked.loc.x + blocked.size.w,
        blocked.loc.y + blocked.size.h,
    );
    let fit_x = |x: i32| x.min(output.w - w).max(0);
    let fit_y = |y: i32| y.min(output.h - h).max(0);
    // Level with the focus: a panel beside it puts its first line of text on
    // the focus's first line; one above or below starts at its left edge.
    let side_y = fit_y(focus.loc.y - 6);
    let stack_x = fit_x(focus.loc.x.min(bx.max(0)));
    // (where, beside rather than stacked)
    let candidates = [
        (Point::from((br + GUTTER, side_y)), true),
        (Point::from((stack_x, bb + GUTTER)), false),
        (Point::from((bx - GUTTER - w, side_y)), true),
        (Point::from((stack_x, by - GUTTER - h)), false),
    ];
    let screen = Rectangle::from_size(output);
    let centre: Point<i32, Logical> =
        Point::from((focus.loc.x + focus.size.w / 2, focus.loc.y + focus.size.h / 2));
    let reach = |(p, beside): (Point<i32, Logical>, bool)| {
        let gap = if beside {
            (p.x - centre.x).max(centre.x - (p.x + w))
        } else {
            (p.y - centre.y).max(centre.y - (p.y + h))
        };
        (level && !beside, gap)
    };
    for clear_of_others in [true, false] {
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
        if let Some((p, _)) = best {
            return p;
        }
    }
    let mut y = bb + GUTTER;
    if y + h > output.h {
        let above = by - GUTTER - h;
        y = if above >= 0 { above } else { (output.h - h).max(0) };
    }
    (fit_x(bx), y).into()
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

    fn pick(r: Rectangle<i32, Logical>, label: &str) -> Option<Pick> {
        Some(Pick {
            rect: r,
            label: label.into(),
        })
    }

    fn add(s: &mut AnnotationStore, owner: u64, text: &str) -> Option<AnnotationId> {
        s.create(owner, rect(0, 0, 10, 10), "", text, None)
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
    fn text_is_reduced_at_the_door() {
        // Whatever the caller sends, what is stored is already sanitised --
        // there is no path that renders the raw string.
        let mut s = AnnotationStore::default();
        let id = s
            .create(1, rect(0, 0, 10, 10), "t\u{1b}i\ntle", "a\u{1b}[2Jb\u{0}c", None)
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
        s.create(1, rect(0, 0, 10, 10), &"t".repeat(500), "", None);
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
        let anchor = rect(100, 100, 300, 200);
        s.create(1, anchor, "", "x", pick(rect(110, 150, 200, 20), "b"));
        s.create(1, anchor, "", "x", pick(rect(110, 150, 291, 20), "B"));
        s.create(1, anchor, "", "x", pick(rect(5000, 5000, 20, 20), "B"));
        s.create(1, anchor, "", "x", pick(rect(110, 150, 0, 20), "B"));
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
    fn a_pick_label_is_one_or_two_letters_or_digits() {
        assert_eq!(pick_label("b").as_deref(), Some("B"));
        assert_eq!(pick_label("12").as_deref(), Some("12"));
        for bad in ["", "ABC", "é", " A", "A)", "\u{1b}"] {
            assert_eq!(pick_label(bad), None, "{bad:?}");
        }
        // The store checks again, whatever the door did.
        let mut s = AnnotationStore::default();
        s.create(1, rect(0, 0, 100, 100), "", "x", pick(rect(1, 1, 5, 5), "<>"));
        assert!(s.iter().next().unwrap().1.pick.is_none());
    }

    #[test]
    fn a_pick_cut_off_by_the_region_cap_is_not_drawn() {
        // The capped region slides to keep a pick, but a pick larger than the
        // cap cannot be kept whole: it goes unmarked rather than half-marked.
        let out: Size<i32, Logical> = (900, 900).into();
        let mut s = AnnotationStore::default();
        s.create(
            1,
            rect(0, 0, 900, 900),
            "t",
            "x",
            pick(rect(10, 300, 100, 400), "A"),
        );
        let a = s.iter().next().unwrap().1;
        let l = lay_out(a, out, (0, 0).into(), &[]).unwrap();
        assert!(l.pick.is_none());
        assert!(l.card.chip.is_empty(), "no marker, no chip");
    }

    /// ADR 0040: annotations are capture-invisible *by construction* --
    /// [`super::super::capture::capture_elements`] builds its own pass list
    /// and never consults this module. There is no GL context in a unit test
    /// to compare two rendered lists, so the assertion is made where the
    /// property actually lives: in the source of the capture pass. The pick
    /// marker (ADR 0054) is built by `draw` in this module, so it is covered
    /// by the same assertion.
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
    fn the_leader_runs_straight_when_it_can_and_square_when_it_cannot() {
        // Panel to the right, level with the focus: one straight run.
        let path = leader(
            rect(100, 100, 200, 100),
            rect(100, 140, 50, 16),
            rect(320, 120, 200, 80),
        )
        .unwrap();
        assert!(path.iter().all(|p| p.y == path[0].y), "{path:?}");
        assert_eq!((path[0].x, path[3].x), (300, 320));
        // Panel below and to the side: out, along, in -- every run on an axis.
        let path = leader(
            rect(100, 100, 40, 40),
            rect(100, 100, 40, 40),
            rect(400, 300, 200, 60),
        )
        .unwrap();
        for w in path.windows(2) {
            assert!(w[0].x == w[1].x || w[0].y == w[1].y, "diagonal run in {path:?}");
        }
        // Touching: nothing to join.
        assert!(leader(rect(0, 0, 40, 40), rect(0, 0, 40, 40), rect(40, 0, 40, 10)).is_none());
        // Overlapping, which only the last-resort placement produces: none.
        assert!(leader(rect(0, 0, 40, 40), rect(0, 0, 40, 40), rect(10, 10, 40, 10)).is_none());
        // Degenerate input is refused rather than drawn.
        assert!(leader(rect(0, 0, 0, 0), rect(0, 0, 0, 0), rect(0, 300, 10, 10)).is_none());
    }

    #[test]
    fn a_capped_region_slides_to_keep_its_pick() {
        // 1440x960 logical, so the cap is 480x320. A quiz whose anchor runs
        // 500 tall with the pick near the bottom keeps the pick on screen.
        let out: Size<i32, Logical> = (1440, 960).into();
        let anchor = rect(100, 100, 400, 500);
        let pick_r = rect(110, 520, 200, 20);
        let r = drawable_region(anchor, out, Some(pick_r)).unwrap();
        assert_eq!((r.size.w, r.size.h), (400, 320), "the cap is unchanged");
        assert!(r.contains_rect(pick_r), "{r:?}");
        assert!(anchor.contains_rect(r), "and it stays inside the anchor");
        // A pick that already fits leaves the top-left where it was.
        let r = drawable_region(anchor, out, Some(rect(110, 150, 200, 20))).unwrap();
        assert_eq!(r.loc, anchor.loc);

        // End to end: the store keeps the pick and the layout draws it.
        let mut s = AnnotationStore::default();
        s.create(1, anchor, "Paris", "", pick(pick_r, "B")).unwrap();
        let a = s.iter().next().unwrap().1;
        let l = lay_out(a, out, (0, 0).into(), &[]).unwrap();
        assert_eq!(l.pick.map(|p| p.rect), Some(pick_r));
    }

    #[test]
    fn a_marker_cannot_be_made_to_frame_the_screen() {
        let out: Size<i32, Logical> = (1920, 1080).into();
        // A caller asking to outline everything gets a third of it, on-screen.
        let r = drawable_region(rect(-500, -500, 9000, 9000), out, None).unwrap();
        assert_eq!((r.loc.x, r.loc.y), (0, 0));
        assert_eq!((r.size.w, r.size.h), (640, 360));
        // A real OCR region is untouched.
        let r = drawable_region(rect(300, 200, 400, 60), out, None).unwrap();
        assert_eq!((r.loc.x, r.loc.y, r.size.w, r.size.h), (300, 200, 400, 60));
        // Entirely off this output: nothing to draw.
        assert!(drawable_region(rect(5000, 5000, 100, 100), out, None).is_none());
    }

    #[test]
    fn an_absurd_anchor_is_reduced_at_the_door() {
        // The anchor is drawn now, not just used for placement, so a socket
        // caller must not be able to overflow the additions in `slots`,
        // `leader` and `place`.
        let mut store = AnnotationStore::default();
        let huge = Rectangle::new(
            Point::from((i32::MAX, i32::MIN)),
            Size::from((i32::MAX, i32::MAX)),
        );
        let id = store.create(1, huge, "", "hi", pick(huge, "A")).unwrap();
        let a = &store.live[&id];
        assert_eq!(a.anchor.loc.x, MAX_COORD);
        assert_eq!(a.anchor.loc.y, -MAX_COORD);
        assert_eq!(a.anchor.size.w, MAX_COORD);
        assert!(a.anchor.loc.x.checked_add(a.anchor.size.w).is_some());
        // Both clamp the same way, so the pick is still inside.
        assert_eq!(a.pick.as_ref().map(|p| p.rect), Some(a.anchor));
    }

    const OUT: (i32, i32) = (1920, 1080);

    fn placed(
        blocked: Rectangle<i32, Logical>,
        size: (i32, i32),
        taken: &[Rectangle<i32, Logical>],
    ) -> Rectangle<i32, Logical> {
        let size = Size::from(size);
        Rectangle::new(place(blocked, blocked, false, size, OUT.into(), taken), size)
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
        let at = place(r, focus, true, (320, 120).into(), OUT.into(), &[]);
        assert_eq!(at.y, 480 - 6);
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
        let p = place(
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
        s.create(1, rect(2020, 100, 200, 300), "", "hello", None);
        let a = s.iter().next().unwrap().1;
        let l = lay_out(a, OUT.into(), (1920, 0).into(), &[]).unwrap();
        assert_eq!(l.region, Some(rect(100, 100, 200, 300)));
        assert_eq!(l.panel.loc.x, 300 + GUTTER);
    }

    #[test]
    fn columns_follow_the_output_and_balance_the_lines() {
        assert_eq!(
            columns("", "", &"word ".repeat(4), 1920),
            28,
            "short text never widens the panel"
        );
        let long = "word ".repeat(60);
        assert_eq!(
            columns("", "", &long, 1920),
            MAX_COLS.min(columns("", "", &long, 9999))
        );
        assert_eq!(
            columns("", "", &long, 600),
            MIN_COLS,
            "a small output gets the narrowest"
        );
        // Balanced: dropping one more column would cost a line.
        let c = columns("", "", &long, 1920);
        let rows = |c| text::wrap(&long, c).len();
        assert!(c == MIN_COLS || rows(c - 1) > rows(c));
    }

    // ---------------------------------------------------------- scene dump
    //
    // `ABYSS_DUMP_ANNOTATIONS=<dir> cargo test -p abyss annotation_scenes`
    // writes each scene below as a PPM: a fake page, and every raster `draw`
    // produces for it composited in place. It is how placement and styling
    // are judged -- by looking -- and without the variable set it still runs
    // every scene through layout and drawing and checks the invariants.

    /// (anchor, title, text, pick): one `annotation_create`.
    type Note = (Rectangle<i32, Logical>, &'static str, &'static str, Option<Pick>);

    struct Scene {
        name: &'static str,
        out: (i32, i32),
        dev: usize,
        /// Page text: (x, y, line), drawn in grey so the panel has something
        /// real to sit over.
        page: Vec<(i32, i32, &'static str)>,
        notes: Vec<Note>,
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

    fn scenes() -> Vec<Scene> {
        let quiz_anchor = rect(412, 292, 440, 128);
        let pick_c = pick(rect(440, 374, 90, 16), "C");
        vec![
            Scene {
                name: "quiz",
                out: OUT,
                dev: 1,
                page: quiz(),
                notes: vec![(quiz_anchor, "Jupiter", "Jupiter is the largest planet, over twice the mass of all the others combined.", pick_c.clone())],
            },
            Scene {
                name: "quiz-2x",
                out: (1280, 720),
                dev: 2,
                page: quiz(),
                notes: vec![(quiz_anchor, "Jupiter", "Jupiter is the largest planet, over twice the mass of all the others combined.", pick_c)],
            },
            Scene {
                name: "prose",
                out: OUT,
                dev: 1,
                page: prose(),
                notes: vec![(rect(152, 492, 1060, 140), "It asks for consent", "The paragraph is placeholder Latin; it carries no meaning beyond showing where body text goes in a layout.", None)],
            },
            Scene {
                name: "column-right",
                out: OUT,
                dev: 1,
                page: vec![(1560, 300, "Sidebar item one"), (1560, 322, "Sidebar item two"), (1560, 344, "Sidebar item three")],
                notes: vec![(rect(1552, 292, 300, 72), "", "A list of three sidebar entries with no further context.", None)],
            },
            Scene {
                name: "two-panels",
                out: OUT,
                dev: 1,
                page: quiz(),
                notes: vec![
                    (quiz_anchor, "First", "The first of two answers about the same region.", None),
                    (quiz_anchor, "Second", "The second answer has to find somewhere else to go.", None),
                ],
            },
            Scene {
                name: "failure",
                out: OUT,
                dev: 1,
                page: vec![],
                notes: vec![(rect(64, 64, 480, 96), "no answer", "claude: rate limited", None)],
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

    #[test]
    fn annotation_scenes() {
        let dir = std::env::var_os("ABYSS_DUMP_ANNOTATIONS");
        for scene in scenes() {
            let out: Size<i32, Logical> = scene.out.into();
            let mut store = AnnotationStore::default();
            for (anchor, title, body, p) in &scene.notes {
                store.create(1, *anchor, title, body, p.clone());
            }
            let mut taken = Vec::new();
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
                assert!(
                    taken
                        .iter()
                        .all(|t: &Rectangle<i32, Logical>| !t.overlaps(l.panel)),
                    "{}: panels collide",
                    scene.name
                );
                taken.push(l.panel);
                drawn.extend(draw(&l, scene.dev));
            }
            let Some(dir) = &dir else { continue };
            let (w, h) = (scene.out.0 as usize * scene.dev, scene.out.1 as usize * scene.dev);
            let mut img = vec![0u8; w * h * 3];
            for px in img.chunks_mut(3) {
                px.copy_from_slice(&[26, 28, 32]);
            }
            for (x, y, line) in &scene.page {
                for (col, ch) in line.bytes().enumerate() {
                    let g = &super::super::font::FONT[(ch - 0x20) as usize];
                    for (gy, bits) in g.iter().enumerate() {
                        for gx in 0..8 {
                            if bits & (0x80 >> gx) == 0 {
                                continue;
                            }
                            for dy in 0..scene.dev {
                                for dx in 0..scene.dev {
                                    let px = (*x as usize + col * 8 + gx) * scene.dev + dx;
                                    let py = (*y as usize + gy) * scene.dev + dy;
                                    if px < w && py < h {
                                        img[(py * w + px) * 3..][..3].copy_from_slice(&[200, 204, 210]);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // `drawn` is front to back; paint it back to front.
            for (at, r) in drawn.iter().rev() {
                for y in 0..r.h as usize {
                    for x in 0..r.w as usize {
                        let (dx, dy) = (
                            at.x as isize * scene.dev as isize + x as isize,
                            at.y as isize * scene.dev as isize + y as isize,
                        );
                        if dx < 0 || dy < 0 || dx as usize >= w || dy as usize >= h {
                            continue;
                        }
                        let s = &r.px[(y * r.w as usize + x) * 4..][..4];
                        over(&mut img[(dy as usize * w + dx as usize) * 3..][..3], s);
                    }
                }
            }
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            ppm.extend_from_slice(&img);
            std::fs::write(std::path::Path::new(dir).join(format!("{}.ppm", scene.name)), ppm).unwrap();
        }
    }
}
