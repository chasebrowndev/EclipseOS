// SPDX-License-Identifier: AGPL-3.0-only

//! Region in, sentence out (spec §2.1).
//!
//! Capture → OCR → redact → ask, with every stage's failure turned into a
//! string the user can be shown. Nothing here decides *when* to run; that is
//! `main.rs`'s job, and automatic mode's gating is `classify`'s.
//!
//! The two expensive handles — the Wayland capture connection and the OCR
//! binary check — are built on first use rather than at startup. A daemon
//! that refuses to start because tesseract is missing has failed invisibly
//! from the user's seat; one that starts and says so on the first chord has
//! not.

use crate::answer::Answerer;
use crate::capture::Capturer;
use crate::classify::{Gate, Verdict};
use crate::config::Config;
use crate::frame::Region;
use crate::hud::Anchor;
use crate::ocr::{self, Ocr, Tesseract, Word};
use crate::redact;

/// What one pass produced, and how long it earned on screen.
pub struct Answer {
    pub anchor: Anchor,
    pub text: String,
    pub hold_ms: u64,
}

pub struct Pipeline {
    cfg: Config,
    capturer: Option<Capturer>,
    ocr: Option<Tesseract>,
    answerer: Answerer,
    gate: Gate,
    /// The last region asked about, so the expand chord has something to
    /// widen (§3.6).
    last: Option<Region>,
}

impl Pipeline {
    pub fn new(cfg: Config) -> Pipeline {
        let answerer = Answerer::new(&cfg);
        // Every field, not a partial override: leaving two of them on
        // `Default` made them constants in all but name, against the
        // "all tuning values are config" rule.
        let gate = Gate::new(crate::classify::Policy {
            min_query_interval_ms: cfg.rate_limit_ms,
            dedup_ttl_ms: cfg.dedup_ttl_ms,
            min_words: cfg.auto_min_words,
            min_confidence: cfg.auto_min_confidence,
            dedup_similarity: cfg.dedup_similarity,
        });
        Pipeline {
            cfg,
            capturer: None,
            ocr: None,
            answerer,
            gate,
            last: None,
        }
    }

    /// The region the last question was about, so a failure can be reported
    /// next to whatever the user pointed at rather than in a corner.
    pub fn last_region(&self) -> Option<Region> {
        self.last
    }

    /// Every output the compositor showed us, for automatic mode.
    pub fn output_regions(&mut self) -> Vec<Region> {
        match self.capturer() {
            Ok(c) => c.output_regions(),
            Err(_) => Vec::new(),
        }
    }

    fn capturer(&mut self) -> Result<&mut Capturer, String> {
        if self.capturer.is_none() {
            self.capturer = Some(Capturer::connect()?);
        }
        Ok(self.capturer.as_mut().expect("just built"))
    }

    fn ocr(&mut self) -> Result<&mut Tesseract, String> {
        if self.ocr.is_none() {
            self.ocr = Some(Tesseract::new(&self.cfg.tesseract_bin)?.with_lang(&self.cfg.ocr_lang));
        }
        Ok(self.ocr.as_mut().expect("just built"))
    }

    /// Read `region` and return what it says, already redacted, along with
    /// OCR's mean confidence over the words it found and the rectangle the
    /// pixels actually came from. The third value is not the second copy of
    /// the argument: `grab` clamps to the owning output, so the rect read can
    /// be smaller than the rect asked for, and it is the read one an answer
    /// has to be anchored on.
    fn read(&mut self, region: Region) -> Result<(String, f32, Region), String> {
        let frame = self.capturer()?.grab(region)?;
        let origin = frame.origin;
        let words = self.ocr()?.recognise(&frame)?;
        if words.is_empty() {
            // An empty result is a failure, not an empty answer: asking a
            // model about nothing spends a query to be told nothing.
            return Err("no readable text in that region".to_string());
        }
        Ok((
            redact::redact(&ocr::text_of(&words)),
            mean_conf(&words),
            origin,
        ))
    }

    /// The logical rect of the output `region` sits on, by centre containment.
    /// `None` when nothing owns it, in which case the caller has no bound to
    /// clamp against and should stay unclamped rather than guess.
    fn owning_output(&mut self, region: Region) -> Option<Region> {
        let (cx, cy) = (region.x + region.w / 2, region.y + region.h / 2);
        self.output_regions()
            .into_iter()
            .find(|o| cx >= o.x && cx < o.x + o.w && cy >= o.y && cy < o.y + o.h)
    }

    /// Select mode (§2.1): the user dragged this rectangle and wants it
    /// explained. No gate — an explicit request is never rate limited or
    /// deduplicated, because the user asking twice means they meant it.
    pub fn select(&mut self, region: Region) -> Result<Answer, String> {
        // Recorded before the read, not after: a select that fails still told
        // us where the user was looking, and that is where the failure has to
        // be reported.
        self.last = Some(region);
        let (text, _, origin) = self.read(region)?;
        let reply = self.answerer.ask(&text, None)?;
        Ok(self.dress(origin, reply))
    }

    /// Expand (§3.6): the same question with more of the screen around it.
    /// Widening rather than re-selecting is the point — the user has already
    /// told us where to look and is saying "not enough context".
    pub fn expand(&mut self) -> Result<Answer, String> {
        let region = self
            .last
            .ok_or_else(|| "nothing to expand — select a region first".to_string())?;
        let bounds = self.owning_output(region);
        let wider = widen(region, bounds);
        let (text, _, _) = self.read(wider)?;
        self.last = Some(wider);
        let reply = self.answerer.ask(&text, None)?;
        // Anchor on the original selection: the answer is still about what
        // the user pointed at, even though we read more to produce it.
        Ok(self.dress(region, reply))
    }

    /// Automatic mode (§2.2): one unprompted pass over `region`. Returns
    /// `Ok(None)` when the gate declined, which is the common case and not
    /// an error.
    pub fn auto(&mut self, region: Region, now_ms: u64) -> Result<Option<Answer>, String> {
        let (text, conf, origin) = self.read(region)?;
        // Scoped by where it was read, not just what it said. Oracle-Eyes has
        // no output id of any kind, so the output's own geometry is the
        // identity: without it, the same page open on two monitors is one
        // duplicate and the second monitor is suppressed forever.
        match self.gate.consider(scope_of(origin), &text, conf, now_ms) {
            Verdict::Skip(_) => Ok(None),
            Verdict::Ask => {
                let reply = self.answerer.ask(&text, None)?;
                Ok(Some(self.dress(origin, reply)))
            }
        }
    }

    /// Attach the §2.2 display time: long enough to read, bounded at both
    /// ends so a one-word answer does not flash and a long one does not
    /// camp on the screen.
    fn dress(&self, region: Region, text: String) -> Answer {
        let words = text.split_whitespace().count() as u64;
        let hold = self
            .cfg
            .min_display_ms
            .max(words.saturating_mul(self.cfg.ms_per_word))
            .min(self.cfg.max_display_ms);
        Answer {
            anchor: Anchor {
                x: region.x,
                y: region.y,
                w: region.w,
                h: region.h,
            },
            text,
            hold_ms: hold,
        }
    }
}

/// Mean OCR confidence as a proportion, 0.0–1.0. Tesseract's TSV reports
/// 0–100; the gate's threshold is a fraction. Returning the raw average made
/// every read look like 8000% confident and the filter never rejected
/// anything.
fn mean_conf(words: &[Word]) -> f32 {
    if words.is_empty() {
        return 0.0;
    }
    let sum: f32 = words.iter().map(|w| w.conf).sum();
    (sum / words.len() as f32 / 100.0).clamp(0.0, 1.0)
}

/// A stable identity for the screen a read came from, derived from its
/// geometry because there is nothing else to derive it from.
fn scope_of(r: Region) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (r.x, r.y, r.w, r.h).hash(&mut h);
    h.finish()
}

/// Double the region about its centre, kept inside `bounds` when the owning
/// output is known. Clamping to `>= 0` was wrong for any monitor whose
/// logical origin is negative — the left screen in a two-monitor layout — and
/// let a widen walk off onto a neighbour.
fn widen(r: Region, bounds: Option<Region>) -> Region {
    let (dw, dh) = (r.w / 2, r.h / 2);
    let mut out = Region {
        x: r.x - dw,
        y: r.y - dh,
        w: r.w + dw * 2,
        h: r.h + dh * 2,
    };
    match bounds {
        Some(b) => {
            out.x = out.x.max(b.x);
            out.y = out.y.max(b.y);
            out.w = out.w.min(b.x + b.w - out.x).max(0);
            out.h = out.h.min(b.y + b.h - out.y).max(0);
        }
        None => {
            out.x = out.x.max(0);
            out.y = out.y.max(0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widening_grows_about_the_centre_and_never_goes_negative() {
        let w = widen(
            Region {
                x: 100,
                y: 100,
                w: 200,
                h: 100,
            },
            None,
        );
        assert_eq!(
            w,
            Region {
                x: 0,
                y: 50,
                w: 400,
                h: 200
            }
        );
        assert_eq!(w.x + w.w / 2, 200, "centre is preserved once clamped away");
    }

    #[test]
    fn widening_at_the_origin_stays_on_screen() {
        let w = widen(
            Region {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
            None,
        );
        assert_eq!(w.x, 0);
        assert_eq!(w.y, 0);
    }

    #[test]
    fn widening_on_a_monitor_at_negative_x_stays_on_that_monitor() {
        // The left screen of a two-monitor layout lives at x = -1920. A
        // clamp to zero would have thrown the read onto the right screen.
        let left = Region {
            x: -1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let w = widen(
            Region {
                x: -1900,
                y: 40,
                w: 400,
                h: 200,
            },
            Some(left),
        );
        assert_eq!(w.x, -1920, "clamped to the monitor, not to the origin");
        assert!(w.x >= left.x && w.x + w.w <= left.x + left.w);
        assert!(w.y >= left.y && w.y + w.h <= left.y + left.h);
    }

    #[test]
    fn confidence_is_a_fraction_not_a_tesseract_percentage() {
        let words = vec![
            Word {
                text: "a".to_string(),
                conf: 40.0,
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            Word {
                text: "b".to_string(),
                conf: 60.0,
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
        ];
        assert!((mean_conf(&words) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn two_outputs_with_the_same_geometry_story_get_different_scopes() {
        let a = Region {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let b = Region {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        assert_ne!(scope_of(a), scope_of(b));
        assert_eq!(scope_of(a), scope_of(a));
    }

    #[test]
    fn display_time_is_bounded_at_both_ends() {
        let p = Pipeline::new(Config::default());
        let r = Region {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let short = p.dress(r, "yes".to_string());
        assert_eq!(short.hold_ms, p.cfg.min_display_ms);
        let long = p.dress(r, "word ".repeat(1000));
        assert_eq!(long.hold_ms, p.cfg.max_display_ms);
    }

    #[test]
    fn mean_confidence_of_nothing_is_zero_not_a_nan() {
        assert_eq!(mean_conf(&[]), 0.0);
    }
}
