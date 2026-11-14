// SPDX-License-Identifier: AGPL-3.0-only
//! The golden-grid fixture model, its KDL reader and its writer (T-25 §1).
//!
//! ```kdl
//! fixture "cuu-basic" {
//!   desc "CUU moves up and stops at the top margin"
//!   covers "T03-CSI-CURSOR" "VTCORE-15"      // conformance rows / matrix ids (informational)
//!   review "reason the expectation is uncertain"   // optional; listed in REVIEW.kdl
//!   requires "sixel"                          // optional; skipped unless the driver has it
//!   version "1.4.2"                           // optional app version for DA2 / XTVERSION
//!   size rows=5 cols=10
//!   modes-init "?2004" "4"                    // set before the first step
//!   feed #"\e[3;1HX\e[2A"# at=0               // steps run in order; `at` = ms for `now`
//!   resize rows=3 cols=8
//!   checkpoint "mid" { grid { row "X" } }     // expectations evaluated at this point
//!   grid { row "..." }                        // final expectations follow
//!   cursor row=0 col=1 visible=#true shape=0  // 0-based; every property optional
//!   cell row=0 col=0 ch="X" fg="p1" bg="default" ul="single" ulc="#ff0000" attrs="bold,italic" wide="narrow" link="https://x"
//!   replies #"\e[0n"#                         // bytes written to the PTY since the last check
//!   events "title:abc" "bell"                 // events since the last check
//!   mode "?25" 1                              // DECRQM Pm: 0 unknown 1 set 2 reset 3 perm-set 4 perm-reset
//!   scrollback { row "old line" }             // primary history, oldest first
//!   images { row "kitty row=0 col=0 cols=2 rows=1 z=0" }  // placements, oldest first
//!   hold #false                               // synchronized-output presentation hold
//!   alt #true                                 // alternate screen active
//! }
//! ```
//!
//! A `grid` lists the first N rows; every row not listed must be blank (so stray output
//! is caught). Row text skips spacer cells and is right-trimmed. A `replies`/`events` node
//! with no arguments asserts "nothing". Sections that are absent are not asserted.
//! Event strings: `title:<t>`, `icon:<t>`, `cwd:<uri>`, `progress:<state>:<value>`,
//! `notify:<text>`, `bell`, `mark:<A|B|C|D[;n]>`, `clipboard-set:<sel>:<bytes>`,
//! `clipboard-read:<sel>` (a read request main answers under the policy; the term never replies),
//! `var:<name>=<value>` / `var:<name>` (OSC 1337 user variable set / removed),
//! `command:started` / `command:finished` (OSC 133 block transitions, after the marks).

use std::path::PathBuf;

use crate::escapes;
use crate::kdl::{self, Node, Value};

/// A mid-stream step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Feed bytes; `at` sets the driver clock (ms) first.
    Feed {
        /// The bytes.
        bytes: Vec<u8>,
        /// Clock in milliseconds since the fixture epoch.
        at: Option<u64>,
    },
    /// Resize the terminal.
    Resize {
        /// Rows.
        rows: usize,
        /// Columns.
        cols: usize,
    },
    /// Check expectations now.
    Checkpoint {
        /// Name for messages.
        name: String,
        /// What to check.
        expect: Box<Expect>,
    },
}

/// Cursor expectation; unset fields are not asserted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CursorExp {
    /// 0-based row.
    pub row: Option<usize>,
    /// 0-based column.
    pub col: Option<usize>,
    /// Visibility.
    pub visible: Option<bool>,
    /// DECSCUSR shape.
    pub shape: Option<u8>,
}

/// Per-cell expectation; unset fields are not asserted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellExp {
    /// 0-based row.
    pub row: usize,
    /// 0-based column.
    pub col: usize,
    /// Cell text.
    pub ch: Option<String>,
    /// Foreground.
    pub fg: Option<String>,
    /// Background.
    pub bg: Option<String>,
    /// Underline colour.
    pub ulc: Option<String>,
    /// Underline style.
    pub ul: Option<String>,
    /// Attribute list (sorted, comma separated; empty for none).
    pub attrs: Option<String>,
    /// Width class name.
    pub wide: Option<String>,
    /// Hyperlink URI; `Some(None)` asserts no link.
    pub link: Option<Option<String>>,
}

/// A set of expectations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expect {
    /// Visible text rows.
    pub grid: Option<Vec<String>>,
    /// Cursor.
    pub cursor: Option<CursorExp>,
    /// Cells.
    pub cells: Vec<CellExp>,
    /// Reply bytes since the last check.
    pub replies: Option<Vec<u8>>,
    /// Events since the last check.
    pub events: Option<Vec<String>>,
    /// `(mode, DECRQM value)`; mode is `?25` for private, `4` for ANSI.
    pub modes: Vec<(String, u8)>,
    /// Primary scrollback text.
    pub scrollback: Option<Vec<String>>,
    /// Image placements (see [`crate::Snapshot::images`]).
    pub images: Option<Vec<String>>,
    /// Presentation hold.
    pub hold: Option<bool>,
    /// Alternate screen.
    pub alt: Option<bool>,
}

impl Expect {
    /// True when nothing is asserted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Expect::default()
    }
}

/// A parsed fixture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixture {
    /// Id; equals the file stem.
    pub id: String,
    /// Description.
    pub desc: String,
    /// Conformance rows / matrix ids this fixture claims.
    pub covers: Vec<String>,
    /// Why the expectation is uncertain, if it is.
    pub review: Option<String>,
    /// Driver features required to run.
    pub requires: Vec<String>,
    /// Application version for DA2 / XTVERSION.
    pub version: Option<String>,
    /// Initial rows.
    pub rows: usize,
    /// Initial columns.
    pub cols: usize,
    /// Modes set before the first step (`?2004`, `4`).
    pub modes_init: Vec<String>,
    /// Ordered steps.
    pub steps: Vec<Step>,
    /// Final expectations.
    pub expect: Expect,
    /// Source file.
    pub path: PathBuf,
}

/// A fixture load error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureError(pub String);

impl std::fmt::Display for FixtureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FixtureError {}

type R<T> = Result<T, FixtureError>;

fn bad<T>(n: &Node, msg: impl std::fmt::Display) -> R<T> {
    Err(FixtureError(format!(
        "line {}: `{}`: {msg}",
        n.line, n.name
    )))
}

fn str_arg(n: &Node, i: usize) -> R<&str> {
    match n.args.get(i).and_then(Value::as_str) {
        Some(s) => Ok(s),
        None => bad(n, format!("argument {i} must be a string")),
    }
}

fn usize_prop(n: &Node, k: &str) -> R<Option<usize>> {
    match n.prop(k) {
        None => Ok(None),
        Some(Value::Int(i)) if *i >= 0 => Ok(Some(*i as usize)),
        Some(_) => bad(n, format!("`{k}` must be a non-negative integer")),
    }
}

fn str_prop(n: &Node, k: &str) -> R<Option<String>> {
    match n.prop(k) {
        None => Ok(None),
        Some(Value::Str(s)) => Ok(Some(s.clone())),
        Some(_) => bad(n, format!("`{k}` must be a string")),
    }
}

fn known_props(n: &Node, allowed: &[&str]) -> R<()> {
    for (k, _) in &n.props {
        if !allowed.contains(&k.as_str()) {
            return bad(n, format!("unknown property `{k}`"));
        }
    }
    Ok(())
}

fn rows_of(n: &Node) -> R<Vec<String>> {
    let mut v = Vec::new();
    for c in &n.children {
        if c.name != "row" {
            return bad(c, "expected `row`");
        }
        v.push(str_arg(c, 0)?.to_string());
    }
    Ok(v)
}

fn bytes_of(n: &Node) -> R<Vec<u8>> {
    let mut out = Vec::new();
    for i in 0..n.args.len() {
        match escapes::unescape(str_arg(n, i)?) {
            Ok(b) => out.extend(b),
            Err(e) => return bad(n, e),
        }
    }
    Ok(out)
}

fn parse_expect_node(n: &Node, e: &mut Expect) -> R<bool> {
    match n.name.as_str() {
        "grid" => e.grid = Some(rows_of(n)?),
        "scrollback" => e.scrollback = Some(rows_of(n)?),
        "images" => e.images = Some(rows_of(n)?),
        "cursor" => {
            known_props(n, &["row", "col", "visible", "shape"])?;
            e.cursor = Some(CursorExp {
                row: usize_prop(n, "row")?,
                col: usize_prop(n, "col")?,
                visible: n.prop("visible").and_then(Value::as_bool),
                shape: usize_prop(n, "shape")?.map(|v| v as u8),
            });
        }
        "cell" => {
            known_props(
                n,
                &[
                    "row", "col", "ch", "fg", "bg", "ulc", "ul", "attrs", "wide", "link",
                ],
            )?;
            let (Some(row), Some(col)) = (usize_prop(n, "row")?, usize_prop(n, "col")?) else {
                return bad(n, "needs row= and col=");
            };
            let link = match n.prop("link") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(Value::Str(s)) => Some(Some(s.clone())),
                Some(_) => return bad(n, "`link` must be a string or #null"),
            };
            e.cells.push(CellExp {
                row,
                col,
                ch: str_prop(n, "ch")?,
                fg: str_prop(n, "fg")?,
                bg: str_prop(n, "bg")?,
                ulc: str_prop(n, "ulc")?,
                ul: str_prop(n, "ul")?,
                attrs: str_prop(n, "attrs")?,
                wide: str_prop(n, "wide")?,
                link,
            });
        }
        "replies" => e.replies = Some(bytes_of(n)?),
        "events" => {
            let mut v = Vec::new();
            for i in 0..n.args.len() {
                v.push(str_arg(n, i)?.to_string());
            }
            e.events = Some(v);
        }
        "mode" => {
            let m = str_arg(n, 0)?.to_string();
            let Some(v) = n.args.get(1).and_then(Value::as_int) else {
                return bad(n, "second argument must be an integer 0..=4");
            };
            if !(0..=4).contains(&v) {
                return bad(n, "DECRQM value must be 0..=4");
            }
            e.modes.push((m, v as u8));
        }
        "hold" => e.hold = n.args.first().and_then(Value::as_bool),
        "alt" => e.alt = n.args.first().and_then(Value::as_bool),
        _ => return Ok(false),
    }
    Ok(true)
}

impl Fixture {
    /// Parse a fixture file's text.
    ///
    /// # Errors
    /// Syntax errors and schema violations, with line numbers.
    pub fn parse(src: &str, path: PathBuf) -> R<Fixture> {
        let doc = kdl::parse(src).map_err(|e| FixtureError(format!("{}: {e}", path.display())))?;
        let [root] = doc.as_slice() else {
            return Err(FixtureError(format!(
                "{}: exactly one `fixture` node expected",
                path.display()
            )));
        };
        if root.name != "fixture" {
            return Err(FixtureError(format!(
                "{}: top-level node must be `fixture`",
                path.display()
            )));
        }
        Self::from_node(root, path.clone())
            .map_err(|e| FixtureError(format!("{}: {e}", path.display())))
    }

    fn from_node(root: &Node, path: PathBuf) -> R<Fixture> {
        let id = str_arg(root, 0)?.to_string();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !stem.is_empty() && stem != id {
            return bad(root, format!("id `{id}` must equal the file stem `{stem}`"));
        }
        let mut fx = Fixture {
            id,
            desc: String::new(),
            covers: Vec::new(),
            review: None,
            requires: Vec::new(),
            version: None,
            rows: 0,
            cols: 0,
            modes_init: Vec::new(),
            steps: Vec::new(),
            expect: Expect::default(),
            path,
        };
        for n in &root.children {
            match n.name.as_str() {
                "desc" => fx.desc = str_arg(n, 0)?.to_string(),
                "covers" => {
                    for i in 0..n.args.len() {
                        fx.covers.push(str_arg(n, i)?.to_string());
                    }
                }
                "review" => fx.review = Some(str_arg(n, 0)?.to_string()),
                "requires" => {
                    for i in 0..n.args.len() {
                        fx.requires.push(str_arg(n, i)?.to_string());
                    }
                }
                "version" => fx.version = Some(str_arg(n, 0)?.to_string()),
                "size" => {
                    known_props(n, &["rows", "cols"])?;
                    let (Some(r), Some(c)) = (usize_prop(n, "rows")?, usize_prop(n, "cols")?)
                    else {
                        return bad(n, "needs rows= and cols=");
                    };
                    if r == 0 || c == 0 || r > 500 || c > 500 {
                        return bad(n, "rows and cols must be 1..=500");
                    }
                    fx.rows = r;
                    fx.cols = c;
                }
                "modes-init" => {
                    for i in 0..n.args.len() {
                        fx.modes_init.push(str_arg(n, i)?.to_string());
                    }
                }
                "feed" => {
                    known_props(n, &["at"])?;
                    fx.steps.push(Step::Feed {
                        bytes: bytes_of(n)?,
                        at: usize_prop(n, "at")?.map(|v| v as u64),
                    });
                }
                "resize" => {
                    known_props(n, &["rows", "cols"])?;
                    let (Some(r), Some(c)) = (usize_prop(n, "rows")?, usize_prop(n, "cols")?)
                    else {
                        return bad(n, "needs rows= and cols=");
                    };
                    fx.steps.push(Step::Resize { rows: r, cols: c });
                }
                "input" => {
                    return bad(
                        n,
                        "`input` steps need the apoth-term input API, which T-03 does not define yet",
                    );
                }
                "checkpoint" => {
                    let mut e = Expect::default();
                    for c in &n.children {
                        if !parse_expect_node(c, &mut e)? {
                            return bad(c, "not an expectation");
                        }
                    }
                    fx.steps.push(Step::Checkpoint {
                        name: str_arg(n, 0)?.to_string(),
                        expect: Box::new(e),
                    });
                }
                _ => {
                    let mut e = std::mem::take(&mut fx.expect);
                    let known = parse_expect_node(n, &mut e)?;
                    fx.expect = e;
                    if !known {
                        return bad(n, "unknown node");
                    }
                }
            }
        }
        if fx.rows == 0 {
            return bad(root, "missing `size`");
        }
        if !fx.steps.iter().any(|s| matches!(s, Step::Feed { .. })) {
            return bad(root, "no `feed` step");
        }
        Ok(fx)
    }

    /// Render back to KDL. Used by bless mode; the output parses to an equal fixture.
    #[must_use]
    pub fn to_kdl(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!("fixture {} {{\n", kdl::quote(&self.id)));
        if !self.desc.is_empty() {
            o.push_str(&format!("  desc {}\n", kdl::quote(&self.desc)));
        }
        if !self.covers.is_empty() {
            let v: Vec<String> = self.covers.iter().map(|s| kdl::quote(s)).collect();
            o.push_str(&format!("  covers {}\n", v.join(" ")));
        }
        if let Some(r) = &self.review {
            o.push_str(&format!("  review {}\n", kdl::quote(r)));
        }
        if !self.requires.is_empty() {
            let v: Vec<String> = self.requires.iter().map(|s| kdl::quote(s)).collect();
            o.push_str(&format!("  requires {}\n", v.join(" ")));
        }
        if let Some(v) = &self.version {
            o.push_str(&format!("  version {}\n", kdl::quote(v)));
        }
        o.push_str(&format!("  size rows={} cols={}\n", self.rows, self.cols));
        if !self.modes_init.is_empty() {
            let v: Vec<String> = self.modes_init.iter().map(|s| kdl::quote(s)).collect();
            o.push_str(&format!("  modes-init {}\n", v.join(" ")));
        }
        for s in &self.steps {
            match s {
                Step::Feed { bytes, at } => {
                    let at = at.map(|a| format!(" at={a}")).unwrap_or_default();
                    o.push_str(&format!(
                        "  feed {}{at}\n",
                        kdl::raw(&escapes::escape(bytes))
                    ));
                }
                Step::Resize { rows, cols } => {
                    o.push_str(&format!("  resize rows={rows} cols={cols}\n"))
                }
                Step::Checkpoint { name, expect } => {
                    o.push_str(&format!("  checkpoint {} {{\n", kdl::quote(name)));
                    write_expect(&mut o, expect, "    ");
                    o.push_str("  }\n");
                }
            }
        }
        write_expect(&mut o, &self.expect, "  ");
        o.push_str("}\n");
        o
    }
}

fn write_rows(o: &mut String, name: &str, rows: &[String], ind: &str) {
    if rows.is_empty() {
        o.push_str(&format!("{ind}{name}\n"));
        return;
    }
    o.push_str(&format!("{ind}{name} {{\n"));
    for r in rows {
        o.push_str(&format!("{ind}  row {}\n", kdl::quote(r)));
    }
    o.push_str(&format!("{ind}}}\n"));
}

fn write_expect(o: &mut String, e: &Expect, ind: &str) {
    if let Some(g) = &e.grid {
        write_rows(o, "grid", g, ind);
    }
    if let Some(c) = &e.cursor {
        let mut l = format!("{ind}cursor");
        if let Some(v) = c.row {
            l.push_str(&format!(" row={v}"));
        }
        if let Some(v) = c.col {
            l.push_str(&format!(" col={v}"));
        }
        if let Some(v) = c.visible {
            l.push_str(&format!(" visible=#{v}"));
        }
        if let Some(v) = c.shape {
            l.push_str(&format!(" shape={v}"));
        }
        o.push_str(&l);
        o.push('\n');
    }
    for c in &e.cells {
        let mut l = format!("{ind}cell row={} col={}", c.row, c.col);
        for (k, v) in [
            ("ch", &c.ch),
            ("fg", &c.fg),
            ("bg", &c.bg),
            ("ulc", &c.ulc),
            ("ul", &c.ul),
            ("attrs", &c.attrs),
            ("wide", &c.wide),
        ] {
            if let Some(v) = v {
                l.push_str(&format!(" {k}={}", kdl::quote(v)));
            }
        }
        match &c.link {
            None => {}
            Some(None) => l.push_str(" link=#null"),
            Some(Some(u)) => l.push_str(&format!(" link={}", kdl::quote(u))),
        }
        o.push_str(&l);
        o.push('\n');
    }
    if let Some(r) = &e.replies {
        if r.is_empty() {
            o.push_str(&format!("{ind}replies\n"));
        } else {
            o.push_str(&format!("{ind}replies {}\n", kdl::raw(&escapes::escape(r))));
        }
    }
    if let Some(v) = &e.events {
        if v.is_empty() {
            o.push_str(&format!("{ind}events\n"));
        } else {
            let q: Vec<String> = v.iter().map(|s| kdl::quote(s)).collect();
            o.push_str(&format!("{ind}events {}\n", q.join(" ")));
        }
    }
    for (m, v) in &e.modes {
        o.push_str(&format!("{ind}mode {} {v}\n", kdl::quote(m)));
    }
    if let Some(s) = &e.scrollback {
        write_rows(o, "scrollback", s, ind);
    }
    if let Some(s) = &e.images {
        write_rows(o, "images", s, ind);
    }
    if let Some(h) = e.hold {
        o.push_str(&format!("{ind}hold #{h}\n"));
    }
    if let Some(a) = e.alt {
        o.push_str(&format!("{ind}alt #{a}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r##"fixture "demo" {
  desc "d"
  covers "T03-X" "VTCORE-15"
  size rows=3 cols=6
  modes-init "?2004" "4"
  feed #"\e[1mHi\e[0m\r\n"# at=5
  resize rows=2 cols=4
  checkpoint "mid" {
    grid {
      row "Hi"
    }
    replies
  }
  grid {
    row "Hi"
    row ""
  }
  cursor row=1 col=0 visible=#true shape=2
  cell row=0 col=0 ch="H" attrs="bold" fg="p1" link=#null
  replies #"\e[0n"#
  events "title:a b" "bell"
  mode "?25" 1
  scrollback {
  }
  hold #false
  alt #true
}
"##;

    #[test]
    fn parses_every_section() {
        let f = Fixture::parse(SRC, "demo.kdl".into()).unwrap();
        assert_eq!((f.rows, f.cols), (3, 6));
        assert_eq!(f.steps.len(), 3);
        assert_eq!(
            f.steps[0],
            Step::Feed {
                bytes: b"\x1b[1mHi\x1b[0m\r\n".to_vec(),
                at: Some(5)
            }
        );
        assert_eq!(f.expect.replies.as_deref(), Some(&b"\x1b[0n"[..]));
        assert_eq!(f.expect.cursor.as_ref().unwrap().shape, Some(2));
        assert_eq!(f.expect.cells[0].link, Some(None));
        assert_eq!(f.expect.modes, vec![("?25".to_string(), 1)]);
        assert_eq!(f.expect.scrollback, Some(vec![]));
        assert_eq!(f.expect.alt, Some(true));
    }

    #[test]
    fn writer_round_trips() {
        let f = Fixture::parse(SRC, "demo.kdl".into()).unwrap();
        let g = Fixture::parse(&f.to_kdl(), "demo.kdl".into()).unwrap();
        assert_eq!(f, g);
    }

    #[test]
    fn schema_errors_name_the_line() {
        let e = Fixture::parse(
            "fixture \"a\" {\n size rows=1 cols=1\n bogus 1\n feed \"x\"\n}",
            "a.kdl".into(),
        )
        .unwrap_err();
        assert!(e.0.contains("line 3"), "{e}");
        assert!(Fixture::parse("fixture \"a\" { size rows=1 cols=1 }", "a.kdl".into()).is_err());
        assert!(
            Fixture::parse(
                "fixture \"b\" { size rows=1 cols=1; feed \"x\" }",
                "a.kdl".into()
            )
            .is_err()
        );
        assert!(
            Fixture::parse(
                "fixture \"a\" { size rows=0 cols=1; feed \"x\" }",
                "a.kdl".into()
            )
            .is_err()
        );
    }
}
