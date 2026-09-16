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
        let gate = Gate::new(crate::classify::Policy {
            min_query_interval_ms: cfg.rate_limit_ms,
            dedup_ttl_ms: cfg.dedup_ttl_ms,
            ..Default::default()
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
    /// OCR's mean confidence over the words it found.
    fn read(&mut self, region: Region) -> Result<(String, f32), String> {
        let frame = self.capturer()?.grab(region)?;
        let words = self.ocr()?.recognise(&frame)?;
        if words.is_empty() {
            // An empty result is a failure, not an empty answer: asking a
            // model about nothing spends a query to be told nothing.
            return Err("no readable text in that region".to_string());
        }
        Ok((redact::redact(&ocr::text_of(&words)), mean_conf(&words)))
    }

    /// Select mode (§2.1): the user dragged this rectangle and wants it
    /// explained. No gate — an explicit request is never rate limited or
    /// deduplicated, because the user asking twice means they meant it.
    pub fn select(&mut self, region: Region) -> Result<Answer, String> {
        let (text, _) = self.read(region)?;
        self.last = Some(region);
        let reply = self.answerer.ask(&text, None)?;
        Ok(self.dress(region, reply))
    }

    /// Expand (§3.6): the same question with more of the screen around it.
    /// Widening rather than re-selecting is the point — the user has already
    /// told us where to look and is saying "not enough context".
    pub fn expand(&mut self) -> Result<Answer, String> {
        let region = self
            .last
            .ok_or_else(|| "nothing to expand — select a region first".to_string())?;
        let wider = widen(region);
        let (text, _) = self.read(wider)?;
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
        let (text, conf) = self.read(region)?;
        match self.gate.consider(&text, conf, now_ms) {
            Verdict::Skip(_) => Ok(None),
            Verdict::Ask => {
                let reply = self.answerer.ask(&text, None)?;
                Ok(Some(self.dress(region, reply)))
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

fn mean_conf(words: &[Word]) -> f32 {
    if words.is_empty() {
        return 0.0;
    }
    words.iter().map(|w| w.conf).sum::<f32>() / words.len() as f32
}

/// Double the region about its centre, clamped to non-negative origin. The
/// compositor clamps the far edge when it grabs, so there is no screen size
/// to know here.
fn widen(r: Region) -> Region {
    let (dw, dh) = (r.w / 2, r.h / 2);
    Region {
        x: (r.x - dw).max(0),
        y: (r.y - dh).max(0),
        w: r.w + dw * 2,
        h: r.h + dh * 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widening_grows_about_the_centre_and_never_goes_negative() {
        let w = widen(Region {
            x: 100,
            y: 100,
            w: 200,
            h: 100,
        });
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
        let w = widen(Region {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        });
        assert_eq!(w.x, 0);
        assert_eq!(w.y, 0);
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
