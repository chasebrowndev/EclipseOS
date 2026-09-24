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

use crate::answer::{Answerer, Confidence, Reply};
use crate::beacon::{Beacon, Eye};
use crate::capture::Capturer;
use crate::choice::{self, Choice};
use crate::classify::{Gate, Verdict};
use crate::config::Config;
use crate::frame::Region;
use crate::hud::{Anchor, Panel, Pick};
use crate::ocr::{self, Line, Ocr, Tesseract, Word};
use crate::redact;

/// What one pass produced, and how long it earned on screen.
pub struct Answer {
    pub anchor: Anchor,
    pub panel: Panel,
    pub hold_ms: u64,
}

/// One read of the screen: redacted lines, the options among them, OCR's
/// confidence, and the rect the pixels actually came from.
struct Read {
    lines: Vec<Line>,
    options: Vec<Choice>,
    conf: f32,
    origin: Region,
}

/// Which fallback anchor a pass uses when the model named no lines.
#[derive(Clone, Copy)]
enum Fallback {
    /// Everything read: the user pointed at it, so all of it is in question.
    All,
    /// The largest paragraph: automatic mode read a whole screen, and
    /// bracketing the whole screen marks nothing.
    Densest,
    /// A rect the caller already knows is the subject.
    Region(Region),
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
    /// The taskbar's eye (ADR 0055). Held here because only the gate knows
    /// the moment automatic mode found something worth asking about.
    pub beacon: Beacon,
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
            beacon: Beacon::bind(),
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

    /// Read `region`: its lines, already redacted, and the options among
    /// them. `origin` is not a second copy of the argument: `grab` clamps to
    /// the owning output, so the rect read can be smaller than the rect asked
    /// for, and it is the read one an answer has to be anchored inside.
    fn read(&mut self, region: Region) -> Result<Read, String> {
        let frame = self.capturer()?.grab(region)?;
        let origin = frame.origin;
        let words = self.ocr()?.recognise(&frame)?;
        if words.is_empty() {
            // An empty result is a failure, not an empty answer: asking a
            // model about nothing spends a query to be told nothing.
            return Err("no readable text in that region".to_string());
        }
        // Redacted per line: every rule is line-local, and the model sees
        // nothing but these lines.
        let mut lines = ocr::lines_of(&words);
        for l in &mut lines {
            l.text = redact::redact(&l.text);
        }
        let options = choice::detect(&lines);
        Ok(Read {
            lines,
            options,
            conf: mean_conf(&words),
            origin,
        })
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
        let read = self.read(region)?;
        let reply = self.answerer.ask(&read.lines, &read.options, None)?;
        Ok(self.dress(&read, reply, Fallback::All))
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
        let read = self.read(wider)?;
        self.last = Some(wider);
        let reply = self.answerer.ask(&read.lines, &read.options, None)?;
        // The anchor comes from the lines the answer is about, which can now
        // lie in the wider read. Without any, fall back to the original
        // selection: the answer is still about what the user pointed at.
        Ok(self.dress(&read, reply, Fallback::Region(region)))
    }

    /// Automatic mode (§2.2): one unprompted pass over `region`. Returns
    /// `Ok(None)` when the gate declined, which is the common case and not
    /// an error.
    pub fn auto(&mut self, region: Region, now_ms: u64) -> Result<Option<Answer>, String> {
        let read = self.read(region)?;
        let text: Vec<&str> = read.lines.iter().map(|l| l.text.as_str()).collect();
        // Scoped by where it was read, not just what it said. Oracle-Eyes has
        // no output id of any kind, so the output's own geometry is the
        // identity: without it, the same page open on two monitors is one
        // duplicate and the second monitor is suppressed forever.
        match self
            .gate
            .consider(scope_of(read.origin), &text.join("\n"), read.conf, now_ms)
        {
            Verdict::Skip(_) => Ok(None),
            Verdict::Ask => {
                self.beacon.set(Eye::Think);
                let reply = self.answerer.ask(&read.lines, &read.options, None)?;
                Ok(Some(self.dress(&read, reply, Fallback::Densest)))
            }
        }
    }

    /// Turn a validated reply into something to draw: where, what, and for
    /// how long. The §2.2 display time is long enough to read, bounded at
    /// both ends so a one-word answer does not flash and a long one does not
    /// camp on the screen.
    fn dress(&self, read: &Read, reply: Reply, fallback: Fallback) -> Answer {
        let pick = reply
            .choice
            .as_ref()
            .and_then(|c| read.options.iter().find(|o| o.label == *c));
        let anchor = anchor_for(read, &reply.focus, pick.is_some(), fallback);
        let mut detail = reply.detail;
        if reply.confidence == Some(Confidence::Low) {
            detail = if detail.is_empty() {
                "low confidence".to_string()
            } else {
                format!("{detail} (low confidence)")
            };
        }
        let words =
            (reply.headline.split_whitespace().count() + detail.split_whitespace().count()) as u64;
        let hold = self
            .cfg
            .min_display_ms
            .max(words.saturating_mul(self.cfg.ms_per_word))
            .min(self.cfg.max_display_ms);
        Answer {
            anchor: anchor.into(),
            panel: Panel {
                title: reply.headline,
                text: detail,
                pick: pick.map(|o| Pick {
                    label: o.label.clone(),
                    x: o.x,
                    y: o.y,
                    w: o.w,
                    h: o.h,
                }),
            },
            hold_ms: hold,
        }
    }
}

/// Margin around the text an anchor brackets, so the marks sit just outside
/// the glyphs rather than on them.
const ANCHOR_PAD: i32 = 6;

fn rect_of_line(l: &Line) -> Region {
    Region {
        x: l.x,
        y: l.y,
        w: l.w,
        h: l.h,
    }
}

fn rect_of_choice(c: &Choice) -> Region {
    Region {
        x: c.x,
        y: c.y,
        w: c.w,
        h: c.h,
    }
}

/// The smallest rect covering all of `rs`, or an empty one at the origin.
fn bounds_of(rs: impl Iterator<Item = Region>) -> Region {
    rs.reduce(|a, b| {
        let (x1, y1) = ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h));
        let (x, y) = (a.x.min(b.x), a.y.min(b.y));
        Region {
            x,
            y,
            w: x1 - x,
            h: y1 - y,
        }
    })
    .unwrap_or(Region {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    })
}

/// Grow `r` by [`ANCHOR_PAD`], never past the rect that was actually read.
fn pad(r: Region, within: Region) -> Region {
    let x = (r.x - ANCHOR_PAD).max(within.x);
    let y = (r.y - ANCHOR_PAD).max(within.y);
    let x1 = (r.x + r.w + ANCHOR_PAD).min(within.x + within.w);
    let y1 = (r.y + r.h + ANCHOR_PAD).min(within.y + within.h);
    Region {
        x,
        y,
        w: (x1 - x).max(1),
        h: (y1 - y).max(1),
    }
}

/// What the answer is about, as a rect measured from OCR. A pick widens it to
/// the whole question — stem and every option — because the compositor only
/// draws a pick inside its anchor, and "B out of these" needs the these.
fn anchor_for(read: &Read, focus: &[usize], picked: bool, fallback: Fallback) -> Region {
    let named = read
        .lines
        .iter()
        .filter(|l| focus.contains(&l.id))
        .map(rect_of_line);
    let r = if picked {
        let first = read.options.first().map(|o| o.line).unwrap_or(1);
        // The stem: the lines just above the first option, back to the last
        // paragraph break.
        let stem_start = read
            .lines
            .iter()
            .filter(|l| l.id <= first && l.para)
            .map(|l| l.id)
            .max()
            .unwrap_or(1);
        let stem = read
            .lines
            .iter()
            .filter(|l| l.id >= stem_start && l.id < first)
            .map(rect_of_line);
        bounds_of(
            named
                .chain(stem)
                .chain(read.options.iter().map(rect_of_choice)),
        )
    } else if !focus.is_empty() {
        bounds_of(named)
    } else {
        match fallback {
            Fallback::All => bounds_of(read.lines.iter().map(rect_of_line)),
            Fallback::Densest => bounds_of(densest(&read.lines).iter().map(rect_of_line)),
            // Already the user's own rect: no pad, no clamp to the read.
            Fallback::Region(r) => return r,
        }
    };
    pad(r, read.origin)
}

/// The paragraph with the most text in it.
fn densest(lines: &[Line]) -> &[Line] {
    let mut best = &lines[..0];
    let mut best_len = 0;
    let mut start = 0;
    for i in 1..=lines.len() {
        if i == lines.len() || lines[i].para {
            let block = &lines[start..i];
            let len: usize = block.iter().map(|l| l.text.len()).sum();
            if len > best_len {
                (best, best_len) = (block, len);
            }
            start = i;
        }
    }
    best
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

    fn line(id: usize, x: i32, y: i32, w: i32, para: bool, text: &str) -> Line {
        Line {
            id,
            text: text.into(),
            x,
            y,
            w,
            h: 16,
            para,
        }
    }

    fn screen() -> Region {
        Region {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        }
    }

    fn reply(headline: &str, detail: &str, focus: &[usize], choice: Option<&str>) -> Reply {
        Reply {
            headline: headline.into(),
            detail: detail.into(),
            focus: focus.to_vec(),
            choice: choice.map(Into::into),
            confidence: Some(Confidence::High),
        }
    }

    fn read_of(lines: Vec<Line>) -> Read {
        let options = choice::detect(&lines);
        Read {
            lines,
            options,
            conf: 0.9,
            origin: screen(),
        }
    }

    #[test]
    fn display_time_is_bounded_at_both_ends() {
        let p = Pipeline::new(Config::default());
        let r = read_of(vec![line(1, 10, 10, 100, false, "x")]);
        let short = p.dress(&r, reply("yes", "", &[], None), Fallback::All);
        assert_eq!(short.hold_ms, p.cfg.min_display_ms);
        let long = p.dress(
            &r,
            reply("", &"word ".repeat(1000), &[], None),
            Fallback::All,
        );
        assert_eq!(long.hold_ms, p.cfg.max_display_ms);
    }

    #[test]
    fn the_anchor_hugs_the_named_lines_not_the_capture() {
        let r = read_of(vec![
            line(1, 100, 100, 300, false, "nav"),
            line(2, 400, 500, 200, true, "the bit"),
            line(3, 400, 520, 250, false, "in question"),
        ]);
        let a = anchor_for(&r, &[2, 3], false, Fallback::Densest);
        assert_eq!(
            a,
            Region {
                x: 394,
                y: 494,
                w: 262,
                h: 48
            }
        );
    }

    #[test]
    fn automatic_mode_without_focus_brackets_the_densest_paragraph() {
        let r = read_of(vec![
            line(1, 0, 0, 50, false, "Menu"),
            line(
                2,
                300,
                400,
                600,
                true,
                "a long paragraph of real prose that matters",
            ),
            line(3, 300, 420, 600, false, "and its second line goes on"),
            line(4, 0, 1000, 60, true, "footer"),
        ]);
        let a = anchor_for(&r, &[], false, Fallback::Densest);
        assert_eq!((a.x, a.y), (294, 394));
        assert_eq!(a.h, 16 + 20 + 12);
        assert!(a.w < screen().w / 2, "never the whole screen");
    }

    #[test]
    fn a_pick_anchors_on_the_whole_question_and_carries_the_option_rect() {
        let p = Pipeline::new(Config::default());
        let r = read_of(vec![
            line(1, 0, 0, 80, false, "unrelated"),
            line(2, 100, 200, 300, true, "Largest planet?"),
            line(3, 100, 220, 120, false, "A) Mars"),
            line(4, 100, 240, 140, false, "B) Jupiter"),
        ]);
        let a = p.dress(&r, reply("Jupiter", "", &[4], Some("B")), Fallback::All);
        assert_eq!((a.anchor.x, a.anchor.y), (94, 194), "stem to last option");
        assert_eq!(a.anchor.h, 56 + 12);
        let pick = a.panel.pick.expect("picked");
        assert_eq!((pick.label.as_str(), pick.x, pick.y), ("B", 100, 240));
    }

    #[test]
    fn expand_falls_back_to_the_users_own_region() {
        let r = read_of(vec![line(1, 10, 10, 100, false, "x")]);
        let mine = Region {
            x: 5,
            y: 5,
            w: 50,
            h: 50,
        };
        assert_eq!(anchor_for(&r, &[], false, Fallback::Region(mine)), mine);
    }

    #[test]
    fn low_confidence_is_said_out_loud() {
        let p = Pipeline::new(Config::default());
        let r = read_of(vec![line(1, 10, 10, 100, false, "x")]);
        let mut rep = reply("Maybe", "", &[], None);
        rep.confidence = Some(Confidence::Low);
        assert_eq!(p.dress(&r, rep, Fallback::All).panel.text, "low confidence");
    }

    #[test]
    fn mean_confidence_of_nothing_is_zero_not_a_nan() {
        assert_eq!(mean_conf(&[]), 0.0);
    }
}
