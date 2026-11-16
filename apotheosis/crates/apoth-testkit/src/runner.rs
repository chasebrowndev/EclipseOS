// SPDX-License-Identifier: AGPL-3.0-only
//! Runs fixtures against a [`Driver`], in every chunking, and reports readable failures.
//!
//! Chunking (T-25 §1): each `feed` step is delivered whole, in 1-byte chunks, in 7-byte
//! chunks, in 4096-byte chunks and in three seeded random splits. Every run must meet the
//! fixture's expectations and must produce *exactly* the observation of the whole-step run
//! (full snapshot, replies, events), so state the fixture does not assert still has to be
//! chunk-invariant. Seeds derive from the fixture id (FNV-1a) and the round; set
//! `APOTH_FIXTURE_SEED` to mix in another value.
//!
//! Bless mode: `APOTH_BLESS=1` rewrites a fixture's asserted sections (all of grid, cursor,
//! replies and events for a fixture that asserts nothing yet) from the whole-step run.
//! Refused when `CI` is set. Blessing never runs unless the env var is present.

use std::fmt::Write as _;
use std::path::Path;

use crate::fixture::{CellExp, CursorExp, Expect, Fixture, Step};
use crate::snapshot::{CellInfo, Snapshot, diff_bytes, diff_list, diff_rows, row_text};

/// What the harness needs from a terminal. The real implementation is
/// [`crate::adapter::TermDriver`]; tests use [`crate::selftest::MiniTerm`].
pub trait Driver {
    /// A terminal of this size. `version` is the application SemVer for DA2/XTVERSION.
    fn create(rows: usize, cols: usize, version: Option<&str>) -> Self
    where
        Self: Sized;
    /// Feed bytes with the clock at `at_ms` milliseconds past the epoch.
    fn feed(&mut self, bytes: &[u8], at_ms: u64);
    /// A real resize.
    fn resize(&mut self, rows: usize, cols: usize);
    /// Drain the reply queue.
    fn take_replies(&mut self) -> Vec<u8>;
    /// Drain the event queue as fixture event strings.
    fn take_events(&mut self) -> Vec<String>;
    /// Observe the state.
    fn snapshot(&self) -> Snapshot;
    /// Whether the build has a feature a fixture `requires` (for example `sixel`).
    fn has_feature(&self, _name: &str) -> bool {
        false
    }
}

/// How a feed step is cut up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chunking {
    /// One call per step.
    Whole,
    /// Fixed-size chunks.
    Size(usize),
    /// Pseudo-random chunk sizes from a seed.
    Random(u64),
}

impl std::fmt::Display for Chunking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Chunking::Whole => f.write_str("whole"),
            Chunking::Size(n) => write!(f, "chunks of {n}"),
            Chunking::Random(s) => write!(f, "random split (seed {s:#x})"),
        }
    }
}

/// The chunkings run for every fixture, derived from the fixture id.
#[must_use]
pub fn chunkings(id: &str) -> Vec<Chunking> {
    let extra = std::env::var("APOTH_FIXTURE_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let base = fnv1a(id.as_bytes()) ^ extra.rotate_left(32);
    let mut v = vec![
        Chunking::Whole,
        Chunking::Size(1),
        Chunking::Size(7),
        Chunking::Size(4096),
    ];
    for round in 0..3u64 {
        v.push(Chunking::Random(splitmix(base.wrapping_add(round))));
    }
    v
}

fn fnv1a(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h ^= u64::from(x);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Cut points for `len` bytes. Deterministic in `(len, chunking)`.
#[must_use]
pub fn split(len: usize, chunking: Chunking) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut state = match chunking {
        Chunking::Random(s) => s,
        _ => 0,
    };
    while at < len {
        let n = match chunking {
            Chunking::Whole => len,
            Chunking::Size(n) => n.max(1),
            Chunking::Random(_) => {
                state = splitmix(state);
                // Mostly tiny pieces (boundaries inside sequences), sometimes long ones.
                match state % 8 {
                    0..=4 => 1 + (state >> 8) as usize % 4,
                    5..=6 => 1 + (state >> 8) as usize % 24,
                    _ => 1 + (state >> 8) as usize % 300,
                }
            }
        };
        let end = (at + n).min(len);
        out.push((at, end));
        at = end;
    }
    out
}

/// What was observed at one checking point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    /// `final` or the checkpoint name.
    pub name: String,
    /// State.
    pub snap: Snapshot,
    /// Replies drained since the last point.
    pub replies: Vec<u8>,
    /// Events drained since the last point.
    pub events: Vec<String>,
    /// DECRQM answers for the modes the expectation names.
    pub modes: Vec<(String, u8)>,
}

fn decrqm_query(mode: &str) -> Result<Vec<u8>, String> {
    let (private, num) = match mode.strip_prefix('?') {
        Some(n) => (true, n),
        None => (false, mode),
    };
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("bad mode name `{mode}`"));
    }
    Ok(format!("\x1b[{}{num}$p", if private { "?" } else { "" }).into_bytes())
}

/// Parse `CSI [?] n ; v $ y` out of `reply`; returns `v`.
fn parse_decrpm(reply: &[u8], mode: &str) -> Result<u8, String> {
    let text = String::from_utf8_lossy(reply);
    let want = format!(
        "\x1b[{}{};",
        if mode.starts_with('?') { "?" } else { "" },
        mode.trim_start_matches('?')
    );
    let Some(rest) = text.strip_prefix(&want) else {
        return Err(format!(
            "DECRQM {mode}: unexpected reply {}",
            crate::escapes::visible(reply)
        ));
    };
    let Some(v) = rest.strip_suffix("$y") else {
        return Err(format!(
            "DECRQM {mode}: unexpected reply {}",
            crate::escapes::visible(reply)
        ));
    };
    v.parse::<u8>().map_err(|_| {
        format!(
            "DECRQM {mode}: bad value in {}",
            crate::escapes::visible(reply)
        )
    })
}

fn observe<D: Driver>(d: &mut D, name: &str, expect: &Expect, clock: u64) -> Result<Point, String> {
    let snap = d.snapshot();
    let replies = d.take_replies();
    let events = d.take_events();
    let mut modes = Vec::new();
    for (m, _) in &expect.modes {
        d.feed(&decrqm_query(m)?, clock);
        let r = d.take_replies();
        let _ = d.take_events();
        modes.push((m.clone(), parse_decrpm(&r, m)?));
    }
    Ok(Point {
        name: name.to_string(),
        snap,
        replies,
        events,
        modes,
    })
}

/// Execute a fixture's steps with one chunking; return the observation at each checkpoint
/// and at the end (the last element is `final`).
///
/// # Errors
/// A malformed fixture (bad mode name, ...).
pub fn execute<D: Driver>(fx: &Fixture, chunking: Chunking) -> Result<Vec<Point>, String> {
    let mut d = D::create(fx.rows, fx.cols, fx.version.as_deref());
    let mut clock = 0u64;
    for m in &fx.modes_init {
        let q = match m.strip_prefix('?') {
            Some(n) => format!("\x1b[?{n}h"),
            None => format!("\x1b[{m}h"),
        };
        d.feed(q.as_bytes(), clock);
    }
    let _ = d.take_replies();
    let _ = d.take_events();
    let mut points = Vec::new();
    for step in &fx.steps {
        match step {
            Step::Feed { bytes, at } => {
                if let Some(a) = at {
                    clock = *a;
                }
                if bytes.is_empty() {
                    d.feed(&[], clock);
                }
                for (a, b) in split(bytes.len(), chunking) {
                    d.feed(&bytes[a..b], clock);
                }
            }
            Step::Resize { rows, cols } => d.resize(*rows, *cols),
            Step::Checkpoint { name, expect } => points.push(observe(&mut d, name, expect, clock)?),
        }
    }
    points.push(observe(&mut d, "final", &fx.expect, clock)?);
    Ok(points)
}

fn check_cell(exp: &CellExp, snap: &Snapshot, out: &mut String) {
    let Some(cell) = snap.cells.get(exp.row).and_then(|r| r.get(exp.col)) else {
        let _ = writeln!(
            out,
            "cell ({},{}) is outside the {}x{} grid",
            exp.row, exp.col, snap.rows, snap.cols
        );
        return;
    };
    let mut bad = |what: &str, e: &str, a: &str| {
        let _ = writeln!(
            out,
            "cell ({},{}) {what}: expected {e:?}, actual {a:?}",
            exp.row, exp.col
        );
    };
    let cmp = |what: &str, e: &Option<String>, a: &str, bad: &mut dyn FnMut(&str, &str, &str)| {
        if let Some(e) = e
            && e != a
        {
            bad(what, e, a);
        }
    };
    cmp("ch", &exp.ch, &cell.ch, &mut bad);
    cmp("fg", &exp.fg, &cell.fg, &mut bad);
    cmp("bg", &exp.bg, &cell.bg, &mut bad);
    cmp("ulc", &exp.ulc, &cell.ulc, &mut bad);
    cmp("ul", &exp.ul, &cell.ul, &mut bad);
    cmp("attrs", &exp.attrs, &cell.attrs, &mut bad);
    cmp("wide", &exp.wide, cell.wide.name(), &mut bad);
    if let Some(l) = &exp.link
        && *l != cell.link
    {
        bad("link", &format!("{l:?}"), &format!("{:?}", cell.link));
    }
}

/// Compare one point with the expectations; returns the readable mismatch report.
#[must_use]
pub fn verify(expect: &Expect, p: &Point) -> String {
    let mut out = String::new();
    let snap = &p.snap;
    if let Some(g) = &expect.grid {
        let mut want: Vec<String> = g
            .iter()
            .map(|s| s.trim_end_matches(' ').to_string())
            .collect();
        if want.len() > snap.rows {
            let _ = writeln!(
                out,
                "grid lists {} rows but the screen has {}",
                want.len(),
                snap.rows
            );
        } else {
            want.resize(snap.rows, String::new());
            if let Some(d) = diff_rows("grid", &want, &snap.grid_text()) {
                out.push_str(&d);
            }
        }
    }
    if let Some(c) = &expect.cursor {
        let a = snap.cursor;
        if c.row.is_some_and(|v| v != a.row) || c.col.is_some_and(|v| v != a.col) {
            let _ = writeln!(
                out,
                "cursor position: expected row={:?} col={:?}, actual row={} col={}",
                c.row, c.col, a.row, a.col
            );
        }
        if c.visible.is_some_and(|v| v != a.visible) {
            let _ = writeln!(
                out,
                "cursor visible: expected {:?}, actual {}",
                c.visible, a.visible
            );
        }
        if c.shape.is_some_and(|v| v != a.shape) {
            let _ = writeln!(
                out,
                "cursor shape: expected {:?}, actual {}",
                c.shape, a.shape
            );
        }
    }
    for c in &expect.cells {
        check_cell(c, snap, &mut out);
    }
    if let Some(r) = &expect.replies
        && let Some(d) = diff_bytes("replies", r, &p.replies)
    {
        out.push_str(&d);
    }
    if let Some(e) = &expect.events
        && let Some(d) = diff_list("events", e, &p.events)
    {
        out.push_str(&d);
    }
    for ((m, want), (_, got)) in expect.modes.iter().zip(&p.modes) {
        if want != got {
            let _ = writeln!(out, "mode {m}: expected DECRQM {want}, actual {got}");
        }
    }
    if let Some(s) = &expect.scrollback {
        let want: Vec<String> = s
            .iter()
            .map(|x| x.trim_end_matches(' ').to_string())
            .collect();
        if let Some(d) = diff_rows("scrollback", &want, &snap.scrollback) {
            out.push_str(&d);
        }
    }
    if let Some(want) = &expect.images
        && let Some(d) = diff_rows("images", want, &snap.images)
    {
        out.push_str(&d);
    }
    if expect.hold.is_some_and(|h| h != snap.hold) {
        let _ = writeln!(
            out,
            "hold: expected {:?}, actual {}",
            expect.hold, snap.hold
        );
    }
    if expect.alt.is_some_and(|a| a != snap.alt) {
        let _ = writeln!(
            out,
            "alt screen: expected {:?}, actual {}",
            expect.alt, snap.alt
        );
    }
    out
}

/// Outcome of running one fixture.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// All chunkings met the expectations and agreed.
    Pass,
    /// A required feature is missing from the driver.
    Skipped(String),
    /// Failed; the string is the full readable report.
    Fail(String),
}

/// Run a fixture in every chunking.
#[must_use]
pub fn run_fixture<D: Driver>(fx: &Fixture) -> Outcome {
    {
        let probe = D::create(fx.rows, fx.cols, fx.version.as_deref());
        for r in &fx.requires {
            if !probe.has_feature(r) {
                return Outcome::Skipped(format!("requires `{r}`"));
            }
        }
    }
    let mut reference: Option<Vec<Point>> = None;
    for ch in chunkings(&fx.id) {
        let pts = match execute::<D>(fx, ch) {
            Ok(p) => p,
            Err(e) => {
                return Outcome::Fail(format!("fixture `{}` ({}): {e}", fx.id, fx.path.display()));
            }
        };
        let mut report = String::new();
        let mut expects: Vec<&Expect> = fx
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Checkpoint { expect, .. } => Some(&**expect),
                _ => None,
            })
            .collect();
        expects.push(&fx.expect);
        for (e, p) in expects.iter().zip(&pts) {
            let r = verify(e, p);
            if !r.is_empty() {
                let _ = write!(report, "-- at `{}`\n{r}", p.name);
            }
        }
        if report.is_empty() {
            match &reference {
                None => reference = Some(pts),
                Some(base) => {
                    if let Some(d) = first_difference(base, &pts) {
                        report = format!("result depends on chunking: {d}\n");
                    }
                }
            }
        }
        if !report.is_empty() {
            return Outcome::Fail(format!(
                "fixture `{}` ({}) failed with {ch}\n{report}",
                fx.id,
                fx.path.display()
            ));
        }
    }
    Outcome::Pass
}

/// Describe the first way two observation lists differ (chunk-invariance check).
#[must_use]
pub fn first_difference(a: &[Point], b: &[Point]) -> Option<String> {
    for (x, y) in a.iter().zip(b) {
        if x.replies != y.replies {
            return diff_bytes(&format!("replies at `{}`", x.name), &x.replies, &y.replies);
        }
        if x.events != y.events {
            return diff_list(&format!("events at `{}`", x.name), &x.events, &y.events);
        }
        if x.modes != y.modes {
            return Some(format!(
                "mode answers at `{}`: {:?} vs {:?}",
                x.name, x.modes, y.modes
            ));
        }
        if x.snap != y.snap {
            if let Some(d) = diff_rows(
                &format!("grid at `{}`", x.name),
                &x.snap.grid_text(),
                &y.snap.grid_text(),
            ) {
                return Some(d);
            }
            if x.snap.scrollback != y.snap.scrollback {
                return diff_rows("scrollback", &x.snap.scrollback, &y.snap.scrollback);
            }
            return Some(format!(
                "non-text state at `{}` (cursor/attributes/hold/alt) differs: {:?} vs {:?}",
                x.name, x.snap.cursor, y.snap.cursor
            ));
        }
    }
    (a.len() != b.len()).then(|| "different number of checkpoints".to_string())
}

// ---- bless -----------------------------------------------------------------------------

fn blessed(e: &Expect, p: &Point, default_all: bool) -> Expect {
    let mut o = Expect::default();
    if e.grid.is_some() || default_all {
        let mut g = p.snap.grid_text();
        while g.last().is_some_and(String::is_empty) {
            g.pop();
        }
        o.grid = Some(g);
    }
    if let Some(c) = &e.cursor {
        o.cursor = Some(CursorExp {
            row: c.row.map(|_| p.snap.cursor.row),
            col: c.col.map(|_| p.snap.cursor.col),
            visible: c.visible.map(|_| p.snap.cursor.visible),
            shape: c.shape.map(|_| p.snap.cursor.shape),
        });
    } else if default_all {
        o.cursor = Some(CursorExp {
            row: Some(p.snap.cursor.row),
            col: Some(p.snap.cursor.col),
            ..CursorExp::default()
        });
    }
    for c in &e.cells {
        let a: &CellInfo = &p.snap.cells[c.row][c.col];
        o.cells.push(CellExp {
            row: c.row,
            col: c.col,
            ch: c.ch.as_ref().map(|_| a.ch.clone()),
            fg: c.fg.as_ref().map(|_| a.fg.clone()),
            bg: c.bg.as_ref().map(|_| a.bg.clone()),
            ulc: c.ulc.as_ref().map(|_| a.ulc.clone()),
            ul: c.ul.as_ref().map(|_| a.ul.clone()),
            attrs: c.attrs.as_ref().map(|_| a.attrs.clone()),
            wide: c.wide.as_ref().map(|_| a.wide.name().to_string()),
            link: c.link.as_ref().map(|_| a.link.clone()),
        });
    }
    if e.replies.is_some() || default_all {
        o.replies = Some(p.replies.clone());
    }
    if e.events.is_some() || default_all {
        o.events = Some(p.events.clone());
    }
    o.modes = p.modes.clone();
    if e.scrollback.is_some() {
        o.scrollback = Some(p.snap.scrollback.clone());
    }
    if e.images.is_some() {
        o.images = Some(p.snap.images.clone());
    }
    o.hold = e.hold.map(|_| p.snap.hold);
    o.alt = e.alt.map(|_| p.snap.alt);
    o
}

/// Rewrite `fx`'s expectations from the whole-step run. Pure; see [`bless_file`].
///
/// # Errors
/// A malformed fixture.
pub fn bless<D: Driver>(fx: &Fixture) -> Result<Fixture, String> {
    let pts = execute::<D>(fx, Chunking::Whole)?;
    let mut out = fx.clone();
    let default_all = fx.expect.is_empty()
        && fx
            .steps
            .iter()
            .all(|s| !matches!(s, Step::Checkpoint { expect, .. } if !expect.is_empty()));
    let mut i = 0;
    for s in &mut out.steps {
        if let Step::Checkpoint { expect, .. } = s {
            **expect = blessed(expect, &pts[i], false);
            i += 1;
        }
    }
    out.expect = blessed(&fx.expect, &pts[i], default_all);
    Ok(out)
}

/// True when the environment asks for blessing and it is allowed.
///
/// # Errors
/// `APOTH_BLESS` is set while `CI` is.
pub fn bless_requested() -> Result<bool, String> {
    if std::env::var_os("APOTH_BLESS").is_none() {
        return Ok(false);
    }
    if std::env::var_os("CI").is_some() {
        return Err(
            "APOTH_BLESS is refused when CI is set: blessing is never automatic in CI".into(),
        );
    }
    Ok(true)
}

/// Bless the fixture file at `path` in place.
///
/// # Errors
/// I/O, parse and blessing errors.
pub fn bless_file<D: Driver>(path: &Path) -> Result<(), String> {
    if !bless_requested()? {
        return Err("set APOTH_BLESS=1 to bless".into());
    }
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fx = Fixture::parse(&src, path.to_path_buf()).map_err(|e| e.to_string())?;
    let new = bless::<D>(&fx)?;
    std::fs::write(path, new.to_kdl()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Pretty-print a fixture's final state, like `cargo xtask fixture show` would.
///
/// # Errors
/// A malformed fixture.
pub fn show<D: Driver>(fx: &Fixture) -> Result<String, String> {
    let pts = execute::<D>(fx, Chunking::Whole)?;
    let p = pts.last().ok_or("no points")?;
    let mut o = format!("fixture {} ({}x{})\n", fx.id, fx.cols, fx.rows);
    for (y, row) in p.snap.cells.iter().enumerate() {
        let _ = writeln!(o, "{y:>3} |{}|", row_text(row));
    }
    let c = p.snap.cursor;
    let _ = writeln!(
        o,
        "cursor row={} col={} visible={} shape={}",
        c.row, c.col, c.visible, c.shape
    );
    let _ = writeln!(o, "replies {}", crate::escapes::visible(&p.replies));
    let _ = writeln!(o, "events {:?}", p.events);
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_cover_the_input_exactly() {
        for len in [0usize, 1, 5, 4096, 10_000] {
            for ch in [
                Chunking::Whole,
                Chunking::Size(1),
                Chunking::Size(7),
                Chunking::Size(4096),
                Chunking::Random(42),
            ] {
                let s = split(len, ch);
                let mut at = 0;
                for (a, b) in &s {
                    assert_eq!(*a, at);
                    assert!(b > a);
                    at = *b;
                }
                assert_eq!(at, len, "{ch}");
            }
        }
        assert_eq!(split(10, Chunking::Size(1)).len(), 10);
        assert_eq!(split(10, Chunking::Size(7)), vec![(0, 7), (7, 10)]);
    }

    #[test]
    fn random_splits_are_deterministic_and_actually_split() {
        let c = chunkings("abc");
        assert_eq!(c, chunkings("abc"));
        assert_ne!(c, chunkings("abd"));
        assert_eq!(c.len(), 7);
        assert!(split(200, c[4]).len() > 3);
    }

    #[test]
    fn mode_queries_and_replies() {
        assert_eq!(decrqm_query("?25").unwrap(), b"\x1b[?25$p");
        assert_eq!(decrqm_query("4").unwrap(), b"\x1b[4$p");
        assert!(decrqm_query("?x").is_err());
        assert_eq!(parse_decrpm(b"\x1b[?25;1$y", "?25").unwrap(), 1);
        assert_eq!(parse_decrpm(b"\x1b[4;2$y", "4").unwrap(), 2);
        assert!(parse_decrpm(b"\x1b[?25;1$y", "4").is_err());
        assert!(parse_decrpm(b"", "4").is_err());
    }
}
