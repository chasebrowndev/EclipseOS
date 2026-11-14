// SPDX-License-Identifier: AGPL-3.0-only
//! Media hover controls (T-08 §4) and the muted notice (T-16 §3 `audio-unavailable`).
//!
//! Over a media placement while the pointer is on it: a glass disc in the middle with a
//! play or pause mark, and a glass scrub pill along the bottom holding the progress track
//! (position over duration, accent fill). Both fade with the hover; at rest nothing is
//! drawn, so the picture stays clean. The muted notice is a small glass pill at the top
//! right of its pane: a muted-speaker mark and one line of interface copy. It is drawn
//! only; it has no hit target, takes no focus and opens nothing.
//!
//! The marks are shapes ([`IconId::Play`], two rounded bars for pause,
//! [`IconId::SpeakerMuted`]), never font glyphs. All copy here is interface text.

use crate::draw::{DrawList, IconId, Rim};
use crate::geom::{Point, Rect};
use crate::state::{ControlState, glass_faded, label_colour, surface};
use crate::text::{Measure, TextRun};
use crate::tokens::{Palette, Rgba, metrics, radius, size, space};

/// Interface copy of the muted notice.
pub const MUTED_TEXT: &str = "No audio output \u{2014} playing muted";

/// One placement's controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudSpec {
    /// The placement's box.
    pub rect: Rect,
    /// Playing: the disc offers pause; paused or ended: play.
    pub playing: bool,
    /// Position over duration in `0..=1`; `None` without a known duration (no track).
    pub progress: Option<f32>,
    /// Fade level in `0..=1` (0 draws nothing).
    pub alpha: f32,
    /// The disc's state. The whole box is its click target, so the pointer being on the box
    /// is the disc's hover.
    pub control: ControlState,
}

/// Where a placement's controls sit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudLayout {
    /// The play/pause disc, centred; `None` when the box is too small for one.
    pub button: Option<Rect>,
    /// The scrub pill and the track inside it; `None` without room.
    pub scrub: Option<(Rect, Rect)>,
}

/// Lays out the controls in `r`: the disc as large as fits (up to `MEDIA_PLAY`), the scrub
/// pill only where it clears the disc.
pub fn layout(r: Rect) -> HudLayout {
    let short = r.w.min(r.h);
    let d = (short / 2.0).min(metrics::MEDIA_PLAY);
    let d = if d < metrics::MEDIA_PLAY_MIN {
        short - 2.0 * space::XS
    } else {
        d
    };
    let button = (d >= metrics::MEDIA_PLAY_MIN * 0.75)
        .then(|| Rect::new(r.x + (r.w - d) / 2.0, r.y + (r.h - d) / 2.0, d, d));
    let scrub = Rect::new(
        r.x + space::SM,
        r.bottom() - space::SM - metrics::MEDIA_SCRUB_H,
        r.w - 2.0 * space::SM,
        metrics::MEDIA_SCRUB_H,
    );
    let clears = button.is_none_or(|b| scrub.y >= b.bottom() + space::XS);
    let scrub = (clears && scrub.w >= 2.0 * metrics::MEDIA_PLAY_MIN).then(|| {
        let pad = (metrics::MEDIA_SCRUB_H - metrics::MEDIA_TRACK_H) / 2.0;
        let track = Rect::new(
            scrub.x + space::SM,
            scrub.y + pad,
            scrub.w - 2.0 * space::SM,
            metrics::MEDIA_TRACK_H,
        );
        (scrub, track)
    });
    HudLayout { button, scrub }
}

/// The glyph square inside a disc.
fn glyph(b: Rect) -> Rect {
    let g = b.w * metrics::MEDIA_GLYPH;
    Rect::new(b.x + (b.w - g) / 2.0, b.y + (b.h - g) / 2.0, g, g)
}

/// The two pause bars in glyph square `g`.
pub fn pause_bars(g: Rect) -> [Rect; 2] {
    let w = g.w * 0.3;
    let gap = g.w * 0.24;
    let x = g.x + (g.w - 2.0 * w - gap) / 2.0;
    [
        Rect::new(x, g.y, w, g.h),
        Rect::new(x + w + gap, g.y, w, g.h),
    ]
}

pub fn draw(s: &HudSpec, pal: &Palette, out: &mut DrawList) {
    let k = s.alpha.clamp(0.0, 1.0);
    if k <= 0.0 {
        return;
    }
    let l = layout(s.rect);
    if let Some(b) = l.button {
        let (fill, rim) = surface(pal, s.control);
        glass_faded(out, b, radius::PILL, rim.colour, pal, k);
        if fill != pal.cell && fill != Rgba::CLEAR {
            out.rect(
                b,
                radius::PILL,
                fill.scaled(k),
                Some(Rim {
                    width: rim.width,
                    colour: rim.colour.scaled(k),
                }),
            );
        }
        let ink = label_colour(pal, s.control).scaled(k);
        let g = glyph(b);
        if s.playing {
            for bar in pause_bars(g) {
                out.rect(bar, bar.w * 0.3, ink, None);
            }
        } else {
            out.icon(IconId::Play, g, ink);
        }
    }
    if let (Some((scrub, track)), Some(f)) = (l.scrub, s.progress) {
        glass_faded(out, scrub, radius::PILL, pal.cell_rim, pal, k);
        out.rect(track, radius::PILL, pal.track.scaled(k), None);
        let w = track.w * f.clamp(0.0, 1.0);
        if w > 0.0 {
            out.rect(
                Rect::new(track.x, track.y, w.max(track.h), track.h),
                radius::PILL,
                pal.accent.scaled(k),
                None,
            );
        }
    }
}

/// The muted pill over pane frame `pane`: top right, inset clear of the scrollbar.
pub fn muted_rect(pane: Rect, measure: &dyn Measure) -> Rect {
    let w = (muted_text_width(pane, measure) + metrics::MUTED_ICON + space::SM + 2.0 * space::MD)
        .min((pane.w - 2.0 * space::MD).max(0.0));
    Rect::new(
        pane.right() - space::MD - w,
        pane.y + space::MD,
        w,
        metrics::MUTED_H,
    )
}

fn muted_run(pane: Rect) -> TextRun {
    let room = pane.w - 2.0 * space::MD - metrics::MUTED_ICON - space::SM - 2.0 * space::MD;
    TextRun::interface(MUTED_TEXT, size::BODY_SMALL).clipped(room.max(0.0))
}

fn muted_text_width(pane: Rect, measure: &dyn Measure) -> f32 {
    measure.width(&muted_run(pane))
}

/// The muted notice over `pane` at fade level `alpha`.
pub fn draw_muted(
    pane: Rect,
    alpha: f32,
    pal: &Palette,
    measure: &dyn Measure,
    out: &mut DrawList,
) {
    let k = alpha.clamp(0.0, 1.0);
    if k <= 0.0 {
        return;
    }
    let r = muted_rect(pane, measure);
    if r.w <= metrics::MUTED_H {
        return;
    }
    glass_faded(out, r, radius::PILL, pal.cell_rim, pal, k);
    let ic = Rect::new(
        r.x + space::MD,
        r.y + (r.h - metrics::MUTED_ICON) / 2.0,
        metrics::MUTED_ICON,
        metrics::MUTED_ICON,
    );
    out.icon(IconId::SpeakerMuted, ic, pal.text_secondary.scaled(k));
    out.text(
        muted_run(pane),
        Point::new(ic.right() + space::SM, r.y + (r.h - size::BODY_SMALL) / 2.0),
        pal.text.scaled(k),
    );
}
