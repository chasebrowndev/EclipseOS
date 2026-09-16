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

use super::{text, AbyssRenderElement};

/// Text colour: eclipse amber, the same face the rest of the system uses.
const FG: text::Rgba = [1.0, 0.72, 0.20, 1.0];
/// Panel colour: near-black, translucent enough to read the content beneath.
const BG: text::Rgba = [0.02, 0.02, 0.03, 0.82];

/// Columns a panel wraps at. Fixed rather than derived from the anchor: a
/// caller must not be able to choose a width that shoulders other content off
/// the screen.
const COLS: usize = 44;

/// Gap between the anchor rectangle and the panel drawn beside it.
const GUTTER: i32 = 8;

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
                anchor,
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
    store: &AnnotationStore,
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
    let px = (fractional.round() as usize).max(1);

    let mut elements = Vec::new();
    for (_, annotation) in store.iter() {
        if annotation.lines.is_empty() {
            continue;
        }
        let raster = text::rasterize(&annotation.lines, px, FG, BG);
        // The raster is already in physical pixels; its logical size is what
        // placement works in.
        let size: Size<i32, Logical> = (raster.w / px as i32, raster.h / px as i32).into();
        let loc = place(annotation.anchor, size, logical, output_loc);
        let Some(element) = upload(renderer, &raster, loc, px, scale) else {
            // A failed upload drops this one panel rather than the frame.
            continue;
        };
        elements.push(element);
    }
    elements
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

/// Upload a raster to a texture and wrap it as a render element.
///
/// `None` on any GL failure: an annotation is cosmetic and must never be able
/// to take a frame down with it.
fn upload(
    renderer: &mut GlesRenderer,
    raster: &text::Raster,
    loc: Point<i32, Logical>,
    px: usize,
    scale: Scale<f64>,
) -> Option<AbyssRenderElement> {
    // `Abgr8888` is RGBA in memory order on a little-endian host, which is how
    // `text::rasterize` lays the panel out.
    let texture: GlesTexture = renderer
        .import_memory(&raster.px, Fourcc::Abgr8888, (raster.w, raster.h).into(), false)
        .ok()?;
    let buffer = TextureBuffer::from_texture(renderer, texture, px as i32, Transform::Normal, None);
    Some(AbyssRenderElement::Texture(
        TextureRenderElement::from_texture_buffer(
            loc.to_f64().to_physical(scale),
            &buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ),
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
