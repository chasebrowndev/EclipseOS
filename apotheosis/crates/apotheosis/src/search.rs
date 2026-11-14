// SPDX-License-Identifier: AGPL-3.0-only
//! Scrollback search (T-14 §6, SCROLLBACKSEARCH-09..13, -17, -19, -31).
//!
//! * The matcher is a `regex-automata` hybrid (lazy) DFA over logical lines (soft-wrapped
//!   rows joined, `apoth_grid::LineScratch`), so a match may span wraps and page seams.
//!   Literal queries are escaped into the same engine. Modes: smart-case (default),
//!   case-sensitive, insensitive; regex; whole-word; scope `all | screen`.
//! * Hard guards (T-14 §6): the NFA has a compile size limit, the lazy DFA gives up when
//!   it thrashes its cache, and a single line that takes longer than [`LINE_BUDGET`]
//!   stops the run. Each is reported as an error instead of stalling.
//! * A [`Run`] scans newest-first in time-budgeted slices. History matches are cached as
//!   immutable; the active area is re-searched whenever output changed it. Matches are
//!   stored in a frame that follows head-page eviction (a reference pin measures how many
//!   rows left the top), so the current match stays put under output and `n`/`N` are
//!   stable.
//!
//! Query text and pane text are user content: nothing here logs them.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use apoth_grid::{LineScratch, Pin, Screen};
use regex_automata::hybrid::dfa;
use regex_automata::hybrid::regex::{Cache, Regex};
use regex_automata::nfa::thompson;
use regex_automata::util::syntax;
use regex_automata::{Input, MatchErrorKind};

/// NFA size limit for a compiled pattern (T-14 §6 compile size limit).
pub(crate) const NFA_LIMIT: usize = 1 << 20;
/// Lazy-DFA transition cache per search.
const DFA_CACHE: usize = 2 << 20;
/// Longest a single logical line may take before the run reports the pattern over budget.
pub(crate) const LINE_BUDGET: Duration = Duration::from_millis(40);
/// Matches kept; the count shows `N+` past this.
pub(crate) const MAX_HITS: usize = 50_000;
/// Longest query, bytes.
pub(crate) const MAX_QUERY: usize = 1024;

/// Case handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Case {
    /// Insensitive unless the query has an uppercase letter.
    #[default]
    Smart,
    Sensitive,
    Insensitive,
}

/// A visible match: start `(row, col)`, end `(row, col)`, and whether it is current.
pub(crate) type Span = ((usize, usize), (usize, usize), bool);

/// What a search covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Scope {
    /// Scrollback and screen.
    #[default]
    All,
    /// The active screen only.
    Screen,
}

/// A query as the search bar holds it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Query {
    pub(crate) text: String,
    pub(crate) case: Case,
    pub(crate) regex: bool,
    pub(crate) whole_word: bool,
    pub(crate) scope: Scope,
}

impl Query {
    pub(crate) fn insensitive(&self) -> bool {
        match self.case {
            Case::Sensitive => false,
            Case::Insensitive => true,
            Case::Smart => !self.text.chars().any(char::is_uppercase),
        }
    }
}

/// Why a search has no results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SearchError {
    /// The pattern does not compile (message is the engine's, about the pattern only).
    Invalid(String),
    /// The pattern is over the compile size limit or over the time budget.
    TooExpensive,
}

impl SearchError {
    pub(crate) fn message(&self) -> String {
        match self {
            SearchError::Invalid(m) => format!("Invalid pattern: {m}"),
            SearchError::TooExpensive => "Pattern too expensive".to_string(),
        }
    }
}

/// Escape regex metacharacters so a literal query matches itself.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.'
                | '+'
                | '*'
                | '?'
                | '('
                | ')'
                | '|'
                | '['
                | ']'
                | '{'
                | '}'
                | '^'
                | '$'
                | '#'
                | '&'
                | '-'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A compiled query.
pub(crate) struct Matcher {
    re: Regex,
    cache: Cache,
    whole_word: bool,
}

impl std::fmt::Debug for Matcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Matcher").finish_non_exhaustive()
    }
}

impl Matcher {
    /// Compile `q`. The empty query compiles to nothing (`None`).
    pub(crate) fn compile(q: &Query) -> Result<Option<Matcher>, SearchError> {
        if q.text.is_empty() {
            return Ok(None);
        }
        if q.text.len() > MAX_QUERY {
            return Err(SearchError::TooExpensive);
        }
        let pat = if q.regex {
            q.text.clone()
        } else {
            escape(&q.text)
        };
        let re = Regex::builder()
            .syntax(
                syntax::Config::new()
                    .case_insensitive(q.insensitive())
                    .utf8(true)
                    .unicode(true),
            )
            .thompson(thompson::Config::new().nfa_size_limit(Some(NFA_LIMIT)))
            .dfa(
                dfa::Config::new()
                    .cache_capacity(DFA_CACHE)
                    .minimum_cache_clear_count(Some(8))
                    .minimum_bytes_per_state(Some(10)),
            )
            .build(&pat)
            .map_err(|e| {
                use std::error::Error as _;
                let nfa = e
                    .source()
                    .and_then(|s| s.downcast_ref::<thompson::BuildError>());
                if nfa.and_then(thompson::BuildError::size_limit).is_some() {
                    return SearchError::TooExpensive;
                }
                // The deepest cause names the pattern problem; its last line is the reason.
                let mut cause: &dyn std::error::Error = &e;
                while let Some(s) = cause.source() {
                    cause = s;
                }
                let m = cause.to_string();
                let last = m.lines().last().unwrap_or("").trim();
                SearchError::Invalid(last.trim_start_matches("error: ").to_string())
            })?;
        let cache = re.create_cache();
        Ok(Some(Matcher {
            re,
            cache,
            whole_word: q.whole_word,
        }))
    }

    /// Append every match of `text` (byte ranges, ascending, non-empty) to `out`, at most
    /// `max` of them.
    pub(crate) fn find_all(
        &mut self,
        text: &str,
        out: &mut Vec<(usize, usize)>,
        max: usize,
    ) -> Result<(), SearchError> {
        let mut at = 0usize;
        let n = out.len();
        while at <= text.len() && out.len() - n < max {
            let input = Input::new(text).range(at..);
            let m = match self.re.try_search(&mut self.cache, &input) {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    return match e.kind() {
                        MatchErrorKind::GaveUp { .. } | MatchErrorKind::Quit { .. } => {
                            Err(SearchError::TooExpensive)
                        }
                        _ => Err(SearchError::TooExpensive),
                    };
                }
            };
            let (s, e) = (m.start(), m.end());
            if e > s {
                let ok = !self.whole_word
                    || (!text[..s].chars().next_back().is_some_and(is_word)
                        && !text[e..].chars().next().is_some_and(is_word));
                if ok {
                    out.push((s, e));
                }
                at = e;
            } else {
                // Empty match: step one character.
                at = s + text[s..].chars().next().map_or(1, char::len_utf8);
            }
        }
        Ok(())
    }
}

/// One match: first and last cell, `(row, col)` in the run's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Hit {
    pub(crate) start: (i64, u16),
    pub(crate) end: (i64, u16),
}

/// Progress for the search bar.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Status {
    /// Matches found so far.
    pub(crate) count: usize,
    /// 1-based index of the current match.
    pub(crate) current: Option<usize>,
    /// The scan has covered its whole scope.
    pub(crate) done: bool,
    /// More than [`MAX_HITS`] matches.
    pub(crate) capped: bool,
    pub(crate) error: Option<String>,
}

/// One search over one screen.
#[derive(Debug)]
pub(crate) struct Run {
    pub(crate) query: Query,
    m: Option<Matcher>,
    scratch: LineScratch,
    probe: LineScratch,
    ranges: Vec<(usize, usize)>,
    /// History matches, ascending.
    hist: VecDeque<Hit>,
    /// Active-area matches, ascending (recomputed when output changes the screen).
    active: Vec<Hit>,
    /// Rows evicted from the top since the run started (frame row = abs + evicted).
    evicted: i64,
    /// A pin on the newest row and its frame row: measures eviction.
    reference: Option<(Pin, i64)>,
    /// Frame row of the first logical line of the active area at the last refresh.
    boundary: Option<i64>,
    /// History is still to be scanned above this frame row.
    older: Option<i64>,
    /// The current match (frame coordinates of its start).
    cur: Option<(i64, u16)>,
    error: Option<SearchError>,
    capped: bool,
    started: bool,
    alt: bool,
}

impl Run {
    /// A run for `q` (compiled now; a bad pattern gives a run that reports the error).
    pub(crate) fn new(q: Query, alt: bool) -> Run {
        let (m, error) = match Matcher::compile(&q) {
            Ok(m) => (m, None),
            Err(e) => (None, Some(e)),
        };
        Run {
            query: q,
            m,
            scratch: LineScratch::default(),
            probe: LineScratch::default(),
            ranges: Vec::new(),
            hist: VecDeque::new(),
            active: Vec::new(),
            evicted: 0,
            reference: None,
            boundary: None,
            older: None,
            cur: None,
            error,
            capped: false,
            started: false,
            alt,
        }
    }

    /// The screen this run belongs to (primary or alternate).
    pub(crate) fn alt(&self) -> bool {
        self.alt
    }

    fn total(&self) -> usize {
        self.hist.len() + self.active.len()
    }

    fn hit(&self, i: usize) -> Hit {
        if i < self.hist.len() {
            self.hist[i]
        } else {
            self.active[i - self.hist.len()]
        }
    }

    /// Search status for the bar.
    pub(crate) fn status(&self) -> Status {
        Status {
            count: self.total(),
            current: self.cur_index().map(|i| i + 1),
            done: self.done(),
            capped: self.capped,
            error: self.error.as_ref().map(SearchError::message),
        }
    }

    /// Nothing left to scan.
    pub(crate) fn done(&self) -> bool {
        self.error.is_some() || self.m.is_none() || (self.started && self.older.is_none())
    }

    fn cur_index(&self) -> Option<usize> {
        let c = self.cur?;
        let n = self.total();
        let i = (0..n).find(|&i| self.hit(i).start == c)?;
        Some(i)
    }

    /// Scan one logical line starting at absolute row `abs`; returns its next row. Pushes
    /// hits (frame coordinates) to `out` in ascending order.
    fn scan_line(
        &mut self,
        g: &Screen,
        abs: usize,
        out: &mut Vec<Hit>,
    ) -> Result<Option<(usize, usize)>, SearchError> {
        let Some(m) = self.m.as_mut() else {
            return Ok(None);
        };
        let Some(span) = g.logical_line(abs, false, &mut self.scratch) else {
            return Ok(None);
        };
        self.ranges.clear();
        let t0 = Instant::now();
        m.find_all(&self.scratch.text, &mut self.ranges, MAX_HITS)?;
        if t0.elapsed() > LINE_BUDGET {
            return Err(SearchError::TooExpensive);
        }
        for &(s, e) in &self.ranges {
            let (Some(a), Some(b)) = (self.scratch.locate(s), self.scratch.locate(e - 1)) else {
                continue;
            };
            let end_col = (b.1 + b.2 - 1).min(g.cols().saturating_sub(1));
            out.push(Hit {
                start: (a.0 as i64 + self.evicted, a.1 as u16),
                end: (b.0 as i64 + self.evicted, end_col as u16),
            });
        }
        Ok(Some((span.start, span.next)))
    }

    /// First row of the logical line containing `abs`.
    fn line_start(&mut self, g: &Screen, abs: usize) -> usize {
        g.logical_line(abs, false, &mut self.probe)
            .map_or(abs, |sp| sp.start)
    }

    fn fail(&mut self, e: SearchError) {
        self.error = Some(e);
        self.hist.clear();
        self.active.clear();
        self.cur = None;
    }

    /// Bring the run up to date with `g` and scan older history for at most `budget`.
    /// `output`: the screen changed since the last refresh. Returns false when the run
    /// must restart (its reference row left the scrollback).
    pub(crate) fn refresh(&mut self, g: &Screen, output: bool, budget: Duration) -> bool {
        if self.done() && self.error.is_some() {
            return true;
        }
        if self.m.is_none() {
            return true;
        }
        let hist_rows = g.history_rows();
        let total = hist_rows + g.rows();
        if total == 0 {
            return true;
        }
        // Eviction: where is the reference row now?
        if let Some((pin, frame)) = self.reference {
            match g.pin_abs(&pin) {
                Some((abs, _)) => self.evicted = frame - abs as i64,
                None => return false,
            }
        }
        self.reference = g
            .pin_at(total - 1, 0)
            .map(|p| (p, (total - 1) as i64 + self.evicted));
        let ev = self.evicted;
        while self.hist.front().is_some_and(|h| h.start.0 < ev) {
            self.hist.pop_front();
        }
        let l = self.line_start(g, hist_rows) as i64 + ev;
        let first = !self.started;
        self.started = true;
        let mut out = Vec::new();
        if first {
            self.boundary = Some(l);
            self.older = (self.query.scope == Scope::All && l > ev).then_some(l);
        } else if let Some(b) = self.boundary
            && b < l
        {
            // Lines that moved from the active area into history since the last refresh.
            let mut row = (b - ev).max(0) as usize;
            while (row as i64 + ev) < l {
                match self.scan_line(g, row, &mut out) {
                    Ok(Some((_, next))) => row = next,
                    Ok(None) => break,
                    Err(e) => {
                        self.fail(e);
                        return true;
                    }
                }
            }
            if self.query.scope == Scope::All {
                self.hist.extend(out.drain(..));
            }
            self.boundary = Some(l);
        }
        if first || output {
            out.clear();
            let mut row = (l - ev).max(0) as usize;
            while row < total {
                match self.scan_line(g, row, &mut out) {
                    Ok(Some((_, next))) => row = next,
                    Ok(None) => break,
                    Err(e) => {
                        self.fail(e);
                        return true;
                    }
                }
            }
            self.active = out;
        }
        // Older history, newest first, one logical line at a time.
        let t0 = Instant::now();
        while let Some(o) = self.older {
            if o <= ev || self.total() >= MAX_HITS {
                self.capped = self.total() >= MAX_HITS;
                self.older = None;
                break;
            }
            let mut line = Vec::new();
            let row = match self.scan_line(g, (o - ev - 1) as usize, &mut line) {
                Ok(Some((start, _))) => start,
                Ok(None) => 0,
                Err(e) => {
                    self.fail(e);
                    return true;
                }
            };
            for h in line.into_iter().rev() {
                self.hist.push_front(h);
            }
            self.older = Some(row as i64 + ev);
            if t0.elapsed() >= budget {
                break;
            }
        }
        if let Some(c) = self.cur
            && c.0 < ev
        {
            self.cur = None;
        }
        true
    }

    /// Step to the next match toward older rows (`older`) or newer rows, starting from the
    /// current match or, with none, from absolute position `from`. Wraps around.
    pub(crate) fn step(&mut self, older: bool, from: (usize, usize)) -> Option<Hit> {
        let n = self.total();
        if n == 0 {
            return None;
        }
        let pos = self
            .cur
            .unwrap_or((from.0 as i64 + self.evicted, from.1 as u16));
        // First index whose start is > pos (hits are ascending).
        let mut lo = 0;
        let mut hi = n;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.hit(mid).start <= pos {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let i = if older {
            // The last hit strictly before pos.
            let before = if self.cur.is_some() {
                lo.checked_sub(2)
            } else {
                lo.checked_sub(1)
            };
            before.unwrap_or(n - 1)
        } else if lo < n {
            lo
        } else {
            0
        };
        let h = self.hit(i);
        self.cur = Some(h.start);
        Some(h)
    }

    /// The current match in absolute rows.
    pub(crate) fn current(&self) -> Option<((usize, usize), (usize, usize))> {
        let i = self.cur_index()?;
        Some(self.to_abs(self.hit(i)))
    }

    fn to_abs(&self, h: Hit) -> ((usize, usize), (usize, usize)) {
        let ev = self.evicted;
        (
            ((h.start.0 - ev).max(0) as usize, h.start.1 as usize),
            ((h.end.0 - ev).max(0) as usize, h.end.1 as usize),
        )
    }

    /// Matches touching absolute rows `top..top + rows`, as `(start, end, current)`.
    pub(crate) fn visible(&self, top: usize, rows: usize) -> Vec<Span> {
        let ev = self.evicted;
        let (a, b) = (top as i64 + ev, (top + rows) as i64 + ev);
        let cur = self.cur;
        let mut v = Vec::new();
        let n = self.total();
        // Hits are ascending by start; a hit starting before `a` may still reach into view.
        let mut lo = 0;
        let mut hi = n;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.hit(mid).start.0 < a - 8 {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        for i in lo..n {
            let h = self.hit(i);
            if h.start.0 >= b {
                break;
            }
            if h.end.0 < a {
                continue;
            }
            let (s, e) = self.to_abs(h);
            v.push((s, e, cur == Some(h.start)));
        }
        v
    }

    /// Hits strictly after (or, with `older`, before) absolute `(row, col)` without moving
    /// the current match (copy-mode `n`/`N`).
    pub(crate) fn peek(&self, older: bool, from: (usize, usize)) -> Option<(usize, usize)> {
        let pos = (from.0 as i64 + self.evicted, from.1 as u16);
        let n = self.total();
        let h = if older {
            (0..n).rev().map(|i| self.hit(i)).find(|h| h.start < pos)
        } else {
            (0..n).map(|i| self.hit(i)).find(|h| h.start > pos)
        }?;
        Some(self.to_abs(h).0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_grid::Limits;
    use apoth_term::{Term, TermConfig};

    fn term(rows: usize, cols: usize, text: &str) -> Term {
        let mut t = Term::new(TermConfig {
            rows,
            cols,
            scrollback: Limits::default(),
            ..TermConfig::default()
        });
        t.feed(text.as_bytes(), Instant::now());
        t
    }

    fn q(text: &str) -> Query {
        Query {
            text: text.into(),
            ..Query::default()
        }
    }

    fn run_all(t: &Term, query: Query) -> Run {
        let mut r = Run::new(query, false);
        assert!(r.refresh(t.grid(), true, Duration::from_secs(5)));
        r
    }

    #[test]
    fn literal_escape_and_case_modes() {
        let t = term(4, 20, "a.b axb A.B\r\n");
        let r = run_all(&t, q("a.b"));
        assert_eq!(
            r.status().count,
            2,
            "smart-case lower query folds case; `.` is literal"
        );
        let r = run_all(
            &t,
            Query {
                case: Case::Sensitive,
                ..q("a.b")
            },
        );
        assert_eq!(r.status().count, 1);
        let r = run_all(&t, q("A.B"));
        assert_eq!(
            r.status().count,
            1,
            "smart-case: an uppercase letter makes it sensitive"
        );
        let r = run_all(
            &t,
            Query {
                regex: true,
                ..q("a.b")
            },
        );
        assert_eq!(r.status().count, 3);
    }

    #[test]
    fn whole_word() {
        let t = term(3, 30, "cat concat cat_x cat.\r\n");
        let r = run_all(
            &t,
            Query {
                whole_word: true,
                ..q("cat")
            },
        );
        assert_eq!(r.status().count, 2);
    }

    #[test]
    fn matches_span_soft_wraps() {
        // 10 columns: "0123456789abcdef" wraps after 9.
        let t = term(4, 10, "xxxxxxxxhello world\r\n");
        let r = run_all(&t, q("hello"));
        assert_eq!(r.status().count, 1);
        let v = r.visible(0, 4);
        assert_eq!(v.len(), 1);
        let ((sr, sc), (er, ec), _) = v[0];
        assert_eq!((sr, sc), (0, 8));
        assert_eq!(
            (er, ec),
            (1, 2),
            "the match crosses the wrap into the next row"
        );
    }

    #[test]
    fn matches_across_page_seams_in_history() {
        let mut text = String::new();
        for i in 0..3000 {
            text.push_str(&format!("line {i} needle\r\n"));
        }
        let t = term(5, 30, &text);
        let r = run_all(&t, q("needle"));
        assert!(r.done());
        assert_eq!(r.status().count, 3000);
        let mut r = Run::new(q("line 1234 "), false);
        r.refresh(t.grid(), true, Duration::from_secs(5));
        let h = r.step(true, (t.grid().history_rows() + 5, 0)).unwrap();
        let ((row, _), _) = r.to_abs(h);
        assert_eq!(
            apoth_term::abs_row_text(t.grid(), row).unwrap().trim_end(),
            "line 1234 needle"
        );
    }

    #[test]
    fn newest_first_slices_resume() {
        let mut text = String::new();
        for i in 0..2000 {
            text.push_str(&format!("{i} hit\r\n"));
        }
        let t = term(5, 20, &text);
        let mut r = Run::new(q("hit"), false);
        r.refresh(t.grid(), true, Duration::ZERO);
        let first = r.status().count;
        assert!(
            first < 2000 && !r.done(),
            "a zero budget scans one line per slice"
        );
        let mut n = 0;
        while !r.done() && n < 10_000 {
            r.refresh(t.grid(), false, Duration::ZERO);
            n += 1;
        }
        assert_eq!(r.status().count, 2000);
    }

    #[test]
    fn step_wraps_and_tracks_current() {
        let t = term(6, 20, "a1\r\na2\r\na3\r\n");
        let mut r = run_all(&t, q("a"));
        let bottom = (t.grid().history_rows() + 5, 0);
        let h1 = r.step(true, bottom).unwrap();
        assert_eq!(
            r.to_abs(h1).0.0,
            2,
            "the first step from the bottom finds the newest"
        );
        let h2 = r.step(true, bottom).unwrap();
        assert_eq!(r.to_abs(h2).0.0, 1);
        r.step(true, bottom);
        let h4 = r.step(true, bottom).unwrap();
        assert_eq!(r.to_abs(h4).0.0, 2, "wraps around");
        assert_eq!(r.status().current, Some(3));
        let h5 = r.step(false, bottom).unwrap();
        assert_eq!(
            r.to_abs(h5).0.0,
            0,
            "forward from the newest wraps to the oldest"
        );
    }

    #[test]
    fn current_match_is_stable_under_output_and_eviction() {
        let mut t = Term::new(TermConfig {
            rows: 4,
            cols: 20,
            scrollback: Limits {
                lines: Some(100),
                ..Limits::default()
            },
            ..TermConfig::default()
        });
        let now = Instant::now();
        for i in 0..50 {
            t.feed(format!("row {i}\r\n").as_bytes(), now);
        }
        t.feed(b"target here\r\n", now);
        let mut r = Run::new(q("target"), false);
        r.refresh(t.grid(), true, Duration::from_secs(1));
        r.step(true, (t.grid().history_rows() + 3, 0)).unwrap();
        for i in 0..400 {
            t.feed(format!("more {i}\r\n").as_bytes(), now);
            if !r.refresh(t.grid(), true, Duration::from_secs(1)) {
                // The reference row was evicted between refreshes: the caller restarts.
                r = Run::new(q("target"), false);
                r.refresh(t.grid(), true, Duration::from_secs(1));
            }
            if let Some(((row, _), _)) = r.current() {
                let text = apoth_term::abs_row_text(t.grid(), row).unwrap();
                assert!(text.starts_with("target"), "current match drifted to {row}");
            }
        }
    }

    #[test]
    fn invalid_and_expensive_patterns_report_errors() {
        let t = term(3, 20, "abc\r\n");
        let r = run_all(
            &t,
            Query {
                regex: true,
                ..q("a(b")
            },
        );
        assert!(matches!(r.status().error, Some(ref e) if e.starts_with("Invalid pattern")));
        let r = run_all(
            &t,
            Query {
                regex: true,
                ..q("\\w{1000}{1000}")
            },
        );
        assert_eq!(r.status().error.as_deref(), Some("Pattern too expensive"));
    }

    #[test]
    fn catastrophic_pattern_errors_within_50ms() {
        // A pattern whose DFA blows up exponentially over a long line: the lazy DFA gives
        // up (cache thrash) or the line budget trips; either way an error, fast.
        let line: String = (0..60_000)
            .map(|i| if (i * 7 + i / 3) % 5 < 2 { '1' } else { '0' })
            .collect();
        let t = term(3, 200, &format!("{line}\r\n"));
        let t0 = Instant::now();
        let r = run_all(
            &t,
            Query {
                regex: true,
                ..q("[01]*1[01]{24}$")
            },
        );
        let took = t0.elapsed();
        assert!(
            took < Duration::from_millis(50) || r.status().error.is_some(),
            "took {took:?}"
        );
        assert!(took < Duration::from_millis(250), "took {took:?}");
    }

    #[test]
    fn screen_scope_skips_history() {
        let t = term(2, 10, "hit\r\nhit\r\nhit\r\nx");
        let r = run_all(
            &t,
            Query {
                scope: Scope::Screen,
                ..q("hit")
            },
        );
        assert_eq!(r.status().count, 1);
        assert!(r.done());
    }

    #[test]
    fn active_area_is_researched_on_output() {
        let mut t = term(4, 20, "foo\r\n");
        let mut r = run_all(&t, q("bar"));
        assert_eq!(r.status().count, 0);
        t.feed(b"bar\r\n", Instant::now());
        r.refresh(t.grid(), true, Duration::from_secs(1));
        assert_eq!(r.status().count, 1);
    }
}
