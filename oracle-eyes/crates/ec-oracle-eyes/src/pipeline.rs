// SPDX-License-Identifier: AGPL-3.0-only

//! Region in, sentence out (spec §2.1).
//!
//! Capture → OCR → redact → ask, with every stage's failure turned into a
//! [`Failure`] whose class the user is shown and whose detail is logged. Nothing here decides *when* to run; that is
//! `main.rs`'s job, and automatic mode's gating is `classify`'s.
//!
//! The two expensive handles — the Wayland capture connection and the OCR
//! binary check — are built on first use rather than at startup. A daemon
//! that refuses to start because tesseract is missing has failed invisibly
//! from the user's seat; one that starts and says so on the first chord has
//! not.

use std::sync::Arc;
use std::time::Instant;

use crate::answer::{Answerer, Ask, Confidence, Reply};
use crate::capture::Capturer;
use crate::choice::{self, Choice};
use crate::classify::{Gate, Verdict};
use crate::config::Config;
use crate::fault::{Failure, Fault};
use crate::frame::{Frame, Region};
use crate::hud::{Anchor, Panel, PanelKind, Pick};
use crate::logsafe::{log_safe, marker_counts};
use crate::ocr::{self, Line, Ocr, Tesseract, Word};
use crate::redact;

/// Where pixels come from. A trait so the whole loop can be driven from
/// fake frames in tests; the real one is the Wayland [`Capturer`].
pub trait Grab {
    fn grab(&mut self, region: Region) -> Result<Frame, String>;
    fn output_regions(&self) -> Vec<Region>;
}

impl Grab for Capturer {
    fn grab(&mut self, region: Region) -> Result<Frame, String> {
        Capturer::grab(self, region)
    }
    fn output_regions(&self) -> Vec<Region> {
        Capturer::output_regions(self)
    }
}

fn capture_failed(e: String) -> Failure {
    Failure::new(Fault::Capture, e)
}

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

/// What asked for a pass. It decides how long the answer stays up and how
/// loudly a failure is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Select,
    Expand,
    Auto,
}

/// A read waiting on its model reply. The reply is fetched off the loop's
/// thread (see `daemon`), and this is what turns it into something to draw.
pub struct Job {
    pub kind: Kind,
    read: Read,
    fallback: Fallback,
}

impl Job {
    pub fn lines(&self) -> &[Line] {
        &self.read.lines
    }
    pub fn options(&self) -> &[Choice] {
        &self.read.options
    }
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
    capturer: Option<Box<dyn Grab>>,
    ocr: Option<Box<dyn Ocr>>,
    asker: Arc<dyn Ask>,
    /// Whether `asker` is the real model call built from `cfg`, and so is
    /// rebuilt when the config changes. A test's fake is left alone.
    asker_from_cfg: bool,
    gate: Gate,
    /// The last region asked about, so the expand chord has something to
    /// widen (§3.6).
    last: Option<Region>,
    /// Automatic mode's last grab, by where and a hash of its pixels. A read
    /// only happens once two grabs in a row agree: the screen has settled
    /// (§2.2 `settle_ms`), so a page mid-scroll or mid-render is not read.
    settling: Option<(Region, u64)>,
    /// The grab automatic mode last read. The same pixels again are skipped
    /// before OCR, which is what keeps an idle screen near-zero cost (§6)
    /// instead of a full OCR every tick.
    read_last: Option<(Region, u64)>,
}

/// A stage's wall time in milliseconds, for the per-stage trace.
fn ms(since: Instant) -> u64 {
    since.elapsed().as_millis() as u64
}

impl Pipeline {
    pub fn new(cfg: Config) -> Pipeline {
        let asker: Arc<dyn Ask> = Arc::new(Answerer::new(&cfg));
        let mut p = Pipeline::with(cfg, None, None, asker);
        p.asker_from_cfg = true;
        p
    }

    /// A new config, live. The model call is rebuilt from it, so the next
    /// question uses the new command and timeout; one already in flight
    /// keeps the [`Answerer`] it started with. The gate keeps what it has
    /// seen — its own tunables only come from `oracle-eyes.kdl`, which is
    /// read once — and so do the capture and OCR handles.
    pub fn set_config(&mut self, cfg: Config) {
        if self.asker_from_cfg {
            self.asker = Arc::new(Answerer::new(&cfg));
        }
        self.cfg = cfg;
    }

    /// A pipeline over given stages, for driving the loop without a
    /// compositor, tesseract or a model.
    #[cfg(test)]
    pub fn with_parts(
        cfg: Config,
        grab: Box<dyn Grab>,
        ocr: Box<dyn Ocr>,
        asker: Arc<dyn Ask>,
    ) -> Pipeline {
        Pipeline::with(cfg, Some(grab), Some(ocr), asker)
    }

    fn with(
        cfg: Config,
        capturer: Option<Box<dyn Grab>>,
        ocr: Option<Box<dyn Ocr>>,
        asker: Arc<dyn Ask>,
    ) -> Pipeline {
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
            capturer,
            ocr,
            asker,
            asker_from_cfg: false,
            gate,
            last: None,
            settling: None,
            read_last: None,
        }
    }

    /// The model call, shared with the worker thread that makes it.
    pub fn asker(&self) -> Arc<dyn Ask> {
        Arc::clone(&self.asker)
    }

    /// Forget what automatic mode has already seen, so switching it on looks
    /// at the screen afresh rather than skipping it as unchanged.
    pub fn reset_auto(&mut self) {
        self.settling = None;
        self.read_last = None;
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

    fn capturer(&mut self) -> Result<&mut Box<dyn Grab>, String> {
        if self.capturer.is_none() {
            self.capturer = Some(Box::new(Capturer::connect()?));
        }
        Ok(self.capturer.as_mut().expect("just built"))
    }

    fn ocr(&mut self) -> Result<&mut Box<dyn Ocr>, String> {
        if self.ocr.is_none() {
            self.ocr = Some(Box::new(
                Tesseract::new(&self.cfg.tesseract_bin)?.with_lang(&self.cfg.ocr_lang),
            ));
        }
        Ok(self.ocr.as_mut().expect("just built"))
    }

    /// Read `region`: its lines, already redacted, and the options among
    /// them. `origin` is not a second copy of the argument: `grab` clamps to
    /// the owning output, so the rect read can be smaller than the rect asked
    /// for, and it is the read one an answer has to be anchored inside.
    fn read(&mut self, region: Region) -> Result<Read, Failure> {
        let t = Instant::now();
        let frame = self
            .capturer()
            .and_then(|c| c.grab(region))
            .map_err(capture_failed)?;
        self.read_frame(&frame, ms(t))
    }

    fn read_frame(&mut self, frame: &Frame, grab_ms: u64) -> Result<Read, Failure> {
        let origin = frame.origin;
        let t = Instant::now();
        let words = self
            .ocr()
            .and_then(|o| o.recognise(frame))
            .map_err(capture_failed)?;
        let ocr_ms = ms(t);
        if words.is_empty() {
            tracing::debug!(grab_ms, ocr_ms, "pipeline: drop, no readable text");
            // An empty result is a failure, not an empty answer: asking a
            // model about nothing spends a query to be told nothing.
            return Err(Failure::new(Fault::NoText, "OCR found no words"));
        }
        // Redacted per line, and the model sees nothing but these lines. The
        // patterns are line-local; the one rule that is not is a label alone
        // on its line (`Password`) with the value on the line below, the way
        // most forms are laid out — that whole next line is masked.
        let t = Instant::now();
        let words = drop_slivers(words, origin);
        let mut lines = ocr::lines_of(&words);
        let mut masked = 0usize;
        let mut under_label = false;
        for l in &mut lines {
            let redacted = if under_label {
                redact::LABELED_LINE.to_string()
            } else {
                redact::redact(&l.text)
            };
            under_label = redact::is_secret_label(&l.text);
            if redacted != l.text {
                masked += 1;
                // Classes and counts only, never the masked text.
                tracing::debug!(line = l.id, markers = %marker_counts(&redacted), "redact: masked");
            }
            l.text = redacted;
        }
        let redact_ms = ms(t);
        if tracing::enabled!(tracing::Level::TRACE) {
            for l in &lines {
                tracing::trace!(
                    id = l.id,
                    x = l.x,
                    y = l.y,
                    w = l.w,
                    h = l.h,
                    para = l.para,
                    text = %log_safe(&l.text),
                    "ocr: line"
                );
            }
        }
        let options = choice::detect(&lines);
        tracing::debug!(
            grab_ms,
            ocr_ms,
            redact_ms,
            words = words.len(),
            lines = lines.len(),
            masked_lines = masked,
            options = options.len(),
            "pipeline: read"
        );
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
    pub fn select(&mut self, region: Region) -> Result<Job, Failure> {
        // Recorded before the read, not after: a select that fails still told
        // us where the user was looking, and that is where the failure has to
        // be reported.
        self.last = Some(region);
        tracing::debug!(?region, "pipeline: select");
        let read = self.read(region)?;
        Ok(Job {
            kind: Kind::Select,
            read,
            fallback: Fallback::All,
        })
    }

    /// Expand (§3.6): the same question with more of the screen around it.
    /// Widening rather than re-selecting is the point — the user has already
    /// told us where to look and is saying "not enough context".
    pub fn expand(&mut self) -> Result<Job, Failure> {
        let region = self.last.ok_or_else(|| {
            Failure::new(
                Fault::NothingSelected,
                "expand with no region selected before it",
            )
        })?;
        let bounds = self.owning_output(region);
        let wider = widen(region, bounds);
        tracing::debug!(?region, ?wider, ?bounds, "pipeline: expand");
        let read = self.read(wider)?;
        self.last = Some(wider);
        // The anchor comes from the lines the answer is about, which can now
        // lie in the wider read. Without any, fall back to the original
        // selection: the answer is still about what the user pointed at.
        Ok(Job {
            kind: Kind::Expand,
            read,
            fallback: Fallback::Region(region),
        })
    }

    /// Automatic mode (§2.2): one unprompted look at `region`. Returns
    /// `Ok(None)` when there is nothing to ask — the screen is still moving,
    /// has not changed since the last read, holds no text, or the gate
    /// declined — which is the common case and not an error.
    pub fn auto(&mut self, region: Region, now_ms: u64) -> Result<Option<Job>, Failure> {
        let t = Instant::now();
        let frame = self
            .capturer()
            .and_then(|c| c.grab(region))
            .map_err(capture_failed)?;
        let grab_ms = ms(t);
        let seen = (frame.origin, pixel_hash(&frame));
        if self.read_last == Some(seen) {
            tracing::trace!(
                grab_ms,
                "pipeline: auto pass skipped, screen unchanged since last read"
            );
            return Ok(None);
        }
        if self.settling != Some(seen) {
            self.settling = Some(seen);
            tracing::debug!(
                grab_ms,
                "pipeline: auto pass waits for the screen to settle"
            );
            return Ok(None);
        }
        // Recorded before the read: a read that fails on these pixels fails
        // the same way next tick, so it is not retried until they change.
        self.read_last = Some(seen);
        let read = match self.read_frame(&frame, grab_ms) {
            Err(e) if e.fault == Fault::NoText => return Ok(None),
            r => r?,
        };
        let text: Vec<&str> = read.lines.iter().map(|l| l.text.as_str()).collect();
        // Scoped by where it was read, not just what it said. Oracle-Eyes has
        // no output id of any kind, so the output's own geometry is the
        // identity: without it, the same page open on two monitors is one
        // duplicate and the second monitor is suppressed forever.
        match self
            .gate
            .consider(scope_of(read.origin), &text.join("\n"), read.conf, now_ms)
        {
            Verdict::Skip(r) => {
                tracing::debug!(reason = %r, total_ms = ms(t), "pipeline: auto pass dropped by gate");
                Ok(None)
            }
            Verdict::Ask => {
                tracing::debug!(read_ms = ms(t), "pipeline: gate asked");
                Ok(Some(Job {
                    kind: Kind::Auto,
                    read,
                    fallback: Fallback::Densest,
                }))
            }
        }
    }

    /// A job's reply, dressed for the screen.
    pub fn finish(&self, job: &Job, reply: Reply) -> Answer {
        self.dress(&job.read, reply, job.fallback)
    }

    /// Turn a validated reply into something to draw: where, what, and for
    /// how long. The §2.2 display time is long enough to read, bounded at
    /// both ends so a one-word answer does not flash and a long one does not
    /// camp on the screen.
    fn dress(&self, read: &Read, reply: Reply, fallback: Fallback) -> Answer {
        let pick = pick_for(read, &reply);
        let anchor = anchor_for(read, &reply.focus, pick, fallback);
        tracing::debug!(
            ?anchor,
            picked = pick.map(|o| o.label.as_str()),
            group = pick.map(|o| o.group),
            "pipeline: anchor"
        );
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
                kind: PanelKind::Answer,
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

/// The option the reply picked, by label. With one list on screen the label
/// is enough. With several, the same label is in each, so the pick is the one
/// whose question holds a line the model named in `focus`; if none or more
/// than one does, there is no pick: an unmarked answer is better than a mark
/// on the other question's option.
fn pick_for<'a>(read: &'a Read, reply: &Reply) -> Option<&'a Choice> {
    let label = reply.choice.as_ref()?;
    let mut found: Vec<&Choice> = read.options.iter().filter(|o| o.label == *label).collect();
    let several = read
        .options
        .iter()
        .any(|o| o.group != read.options[0].group);
    if several && (found.len() > 1 || !reply.focus.is_empty()) {
        found.retain(|o| {
            let (top, at) = stem_of(read, o.group, &[]);
            reply.focus.iter().any(|id| {
                read.lines[top..at].iter().any(|l| l.id == *id)
                    || read
                        .options
                        .iter()
                        .filter(|c| c.group == o.group)
                        .any(|c| read.lines.iter().any(|l| l.id == *id && inside(l, c)))
            })
        });
    }
    if found.len() != 1 {
        tracing::debug!(
            matches = found.len(),
            "pipeline: pick dropped, the label does not say which question"
        );
        return None;
    }
    found.pop()
}

/// Whether line `l` lies within option `c`'s rect: its first line or one of
/// its wrapped lines.
fn inside(l: &Line, c: &Choice) -> bool {
    l.y >= c.y && l.y < c.y + c.h && l.x < c.x + c.w && c.x < l.x + l.w
}

/// How far above its first option a question line may sit, in line
/// pitches, when space (a picture, a diagram) separates the two.
const MAX_STEM_GAP: i32 = 15;
/// Top-to-top distance, in tenths of a pitch, that still joins two
/// paragraphs of one question: a blank line (2.0) and some slack.
const PASSAGE_PITCH: i32 = 30;

/// The stem of option group `group`, as indices `top..at` into `read.lines`:
/// `at` is the group's first option and `top` the first line of its question.
/// `top == at` when no stem was found.
///
/// The stem is, in order:
/// 1. the line right above the first option, if it is set close (no more
///    than one blank line away), or if it reads as a question (`?`, `:`) and
///    sits no more than [`MAX_STEM_GAP`] pitches up — the gap where a
///    picture or diagram goes;
/// 2. and every line above that set close to it: the question's paragraph;
/// 3. and, across single blank lines, any earlier paragraphs up to the
///    topmost line the model named in `focus` — the passage a long question
///    depends on. Geometry alone cannot tell a question's own earlier
///    paragraph from an unrelated one above it; the model, which read both,
///    can.
///
/// No step crosses into another column (no horizontal overlap with the
/// options), goes back up the page, or takes a line of another list.
fn stem_of(read: &Read, group: usize, focus: &[usize]) -> (usize, usize) {
    let lines = &read.lines;
    let opts: Vec<&Choice> = read.options.iter().filter(|o| o.group == group).collect();
    let Some(at) = opts
        .first()
        .and_then(|f| lines.iter().position(|l| l.id == f.line))
    else {
        return (0, 0);
    };
    let pitch = ocr::pitch(lines);
    let (x0, x1) = (
        opts.iter().map(|o| o.x).min().unwrap_or(0),
        opts.iter().map(|o| o.x + o.w).max().unwrap_or(0),
    );
    let usable = |l: &Line| {
        l.x < x1
            && x0 < l.x + l.w
            && !read
                .options
                .iter()
                .any(|o| o.group != group && inside(l, o))
    };
    let step = |i: usize| lines[i].y - lines[i - 1].y;
    let close = |i: usize| step(i) > 0 && step(i) * 10 <= pitch * ocr::PARA_PITCH;
    let near = |i: usize| step(i) > 0 && step(i) * 10 <= pitch * PASSAGE_PITCH;

    if at == 0 || !usable(&lines[at - 1]) {
        return (at, at);
    }
    let asks = {
        let t = lines[at - 1].text.trim_end();
        t.ends_with('?') || t.ends_with(':')
    };
    if !(near(at) || asks && step(at) > 0 && step(at) <= pitch * MAX_STEM_GAP) {
        return (at, at);
    }
    let mut top = at - 1;
    while top > 0 && usable(&lines[top - 1]) && close(top) {
        top -= 1;
    }
    if let Some(want) = focus
        .iter()
        .filter_map(|id| lines[..top].iter().position(|l| l.id == *id))
        .min()
    {
        let mut t = top;
        while t > want && usable(&lines[t - 1]) && near(t) {
            t -= 1;
        }
        if t == want {
            while t > 0 && usable(&lines[t - 1]) && close(t) {
                t -= 1;
            }
            top = t;
        }
    }
    (top, at)
}

/// What the answer is about, as a rect measured from OCR. A pick anchors on
/// the whole question — its stem ([`stem_of`]) and every option of its own
/// list — and on nothing else: the compositor only draws a pick inside its
/// anchor, "B out of these" needs the these, and focus lines elsewhere on the
/// page would stretch the anchor past the compositor's size cap, which trims
/// the options and can drop the pick with them.
fn anchor_for(read: &Read, focus: &[usize], pick: Option<&Choice>, fallback: Fallback) -> Region {
    let r = if let Some(p) = pick {
        let (top, at) = stem_of(read, p.group, focus);
        let stem = read.lines[top..at].iter().map(rect_of_line);
        let opts = read.options.iter().filter(|o| o.group == p.group);
        bounds_of(stem.chain(opts.map(rect_of_choice)))
    } else if !focus.is_empty() {
        // The whole paragraph of every line the answer is about: the model
        // names a line of the question, and the brackets belong around all of it.
        let named = focus
            .iter()
            .filter_map(|id| read.lines.iter().position(|l| l.id == *id))
            .flat_map(|i| {
                let start = read.lines[..=i].iter().rposition(|l| l.para).unwrap_or(0);
                let end = read.lines[i + 1..]
                    .iter()
                    .position(|l| l.para)
                    .map_or(read.lines.len(), |p| i + 1 + p);
                read.lines[start..end].iter().map(rect_of_line)
            });
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

/// Words cut by the edge of the read: a line the selection sliced through
/// comes back as a strip a few pixels tall, read as `E e` or `_`. They touch
/// the top or bottom edge and are under half a typical word's height. Left
/// in, they become a line of their own that the model is asked about and an
/// anchor stretches up to.
fn drop_slivers(words: Vec<Word>, origin: Region) -> Vec<Word> {
    let mut hs: Vec<i32> = words.iter().map(|w| w.h).collect();
    hs.sort_unstable();
    let typical = hs.get(hs.len() / 2).copied().unwrap_or(0);
    let before = words.len();
    let kept: Vec<Word> = words
        .into_iter()
        .filter(|w| {
            let edge = w.y <= origin.y + 1 || w.y + w.h >= origin.y + origin.h - 1;
            !(edge && w.h * 2 < typical)
        })
        .collect();
    if kept.len() < before {
        tracing::debug!(
            dropped = before - kept.len(),
            "ocr: dropped words cut by the edge"
        );
    }
    kept
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

/// A cheap identity for a grab's pixels: equal hashes mean "nothing moved".
fn pixel_hash(f: &Frame) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(&f.pixels);
    h.finish()
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
        let a = anchor_for(&r, &[2, 3], None, Fallback::Densest);
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
        let a = anchor_for(&r, &[], None, Fallback::Densest);
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
    fn a_pick_ignores_focus_lines_outside_the_question() {
        // The model named the search box that asked for the quiz; the
        // brackets still hug the quiz, so the compositor's cap cannot trim
        // the options off the bottom.
        let p = Pipeline::new(Config::default());
        let r = read_of(vec![
            line(
                1,
                300,
                20,
                400,
                false,
                "generate me a multiple choice question",
            ),
            line(2, 200, 400, 300, true, "Capital of France?"),
            line(3, 200, 440, 120, true, "A) London"),
            line(4, 200, 480, 120, true, "B) Paris"),
            line(5, 200, 520, 120, true, "C) Rome"),
            line(6, 200, 560, 120, true, "D) Madrid"),
        ]);
        let a = p.dress(&r, reply("Paris", "", &[1, 4], Some("B")), Fallback::All);
        assert_eq!(a.anchor.y, 394, "starts at the stem, not the search box");
        assert_eq!(a.anchor.y + a.anchor.h, 576 + 6, "ends below D");
    }

    #[test]
    fn a_pick_includes_a_stem_of_several_paragraphs() {
        let p = Pipeline::new(Config::default());
        let r = read_of(vec![
            line(1, 0, 0, 80, false, "unrelated"),
            line(2, 200, 300, 400, true, "Read the passage below."),
            line(3, 200, 328, 400, true, "The cat sat on the mat."),
            line(4, 200, 356, 300, true, "Where did the cat sit?"),
            line(5, 200, 390, 120, true, "A) On a mat"),
            line(6, 200, 420, 120, true, "B) On a bed"),
        ]);
        let a = p.dress(&r, reply("A", "", &[6], Some("A")), Fallback::All);
        assert_eq!(a.anchor.y, 294, "starts at the passage, not the stem alone");
        assert_eq!(
            a.anchor.y + a.anchor.h,
            420 + 16 + 6,
            "ends below the last option"
        );
    }

    #[test]
    fn a_focus_line_brackets_its_whole_paragraph() {
        let r = read_of(vec![
            line(1, 0, 0, 80, false, "Menu"),
            line(2, 300, 400, 200, true, "What is the capital"),
            line(3, 300, 420, 500, false, "of France, given the map above?"),
            line(4, 300, 440, 100, false, "Answer soon."),
            line(5, 300, 700, 60, true, "footer"),
        ]);
        let a = anchor_for(&r, &[3], None, Fallback::All);
        assert_eq!((a.x, a.y), (294, 394));
        assert_eq!((a.w, a.h), (500 + 12, 56 + 12), "lines 2-4, not 5");
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
        assert_eq!(anchor_for(&r, &[], None, Fallback::Region(mine)), mine);
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
