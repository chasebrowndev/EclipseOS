// SPDX-License-Identifier: AGPL-3.0-only
//! The match language (T-11 §7): parse once, evaluate many times against anything that can
//! answer [`Fields`]. One grammar for rule `where`, profile matches, socket targets and
//! template conditions.
//!
//! ```text
//! expr   := or
//! or     := and ("or" and)*
//! and    := unary ("and" unary)*
//! unary  := "not" unary | "(" expr ")" | term
//! term   := STATE | field ":" pattern | ("var" | "env") ":" name ":" pattern
//! pattern:= "~" regex | numeric-cmp (numeric fields) | glob      (quotes allowed)
//! ```
//!
//! `and` binds tighter than `or`. Globs are anchored (`*` any run, `?` one character, `\`
//! escapes). Numeric fields take `N`, `=N`, `!N`, `>N`, `>=N`, `<N`, `<=N`; `duration` also
//! takes `ms`, `s`, `m`, `h` suffixes and compares in milliseconds. `id` also takes the
//! relative tokens. A missing field never matches. It is a filter language: no assignment,
//! calls or loops, and every input is bounded (length, nesting, regex size).

use std::borrow::Cow;
use std::sync::Arc;

use regex_automata::meta::Regex;

use crate::lang::{MATCH_BARE, MATCH_FIELDS, MATCH_MAX_DEPTH, MAX_REGEX_BYTES};

/// Longest match expression accepted.
pub const MAX_EXPR_BYTES: usize = 4096;

/// `progress-changed`'s `state` values.
pub const PROGRESS_STATES: &[&str] = &["remove", "value", "error", "indeterminate", "paused"];

/// Fields whose patterns are numeric comparisons.
pub const NUMERIC_FIELDS: &[&str] = &["pid", "tab", "window", "status", "duration"];

/// An event field name (T-11 §7 "plus event fields"): any field some event carries.
fn event_field(name: &str) -> Option<apoth_action::event::EventField> {
    apoth_action::EventKind::ALL
        .iter()
        .find_map(|k| k.fields().iter().find(|f| f.name == name).copied())
}

/// The field compares numerically: a listed numeric field or an integer / duration event
/// field.
fn is_numeric(field: &str) -> bool {
    use apoth_action::event::FieldType;
    NUMERIC_FIELDS.contains(&field)
        || event_field(field).is_some_and(|f| matches!(f.ty, FieldType::Int | FieldType::Duration))
}

/// The field is in milliseconds (accepts `ms`/`s`/`m`/`h`).
fn is_duration(field: &str) -> bool {
    field == "duration"
        || event_field(field).is_some_and(|f| f.ty == apoth_action::event::FieldType::Duration)
}

/// Relative tokens `id:` accepts (socket targeting, T-11 §7).
pub const RELATIVE: &[&str] = &[
    "self", "last", "next", "prev", "left", "right", "up", "down", "marked",
];

/// What an expression is evaluated against.
pub trait Fields {
    /// A field's text (`title`, `cwd`, `cmdline`, `process`, `host`, `user`, `profile`,
    /// `workspace`, `command`, numeric fields as decimal). `None` = not known here.
    fn text(&self, field: &str) -> Option<Cow<'_, str>>;
    /// A numeric field (`duration` in milliseconds). Defaults to parsing [`Fields::text`].
    fn number(&self, field: &str) -> Option<i64> {
        self.text(field).and_then(|t| t.trim().parse().ok())
    }
    /// A state word (`focused`, `active`, `zoomed`, `bell`, `running`).
    fn state(&self, _word: &str) -> bool {
        false
    }
    /// A user variable.
    fn var(&self, _name: &str) -> Option<Cow<'_, str>> {
        None
    }
    /// An environment variable of the pane.
    fn env(&self, _name: &str) -> Option<Cow<'_, str>> {
        None
    }
    /// A relative token (`self`, `last`, …) applied to the candidate.
    fn relative(&self, _token: &str) -> bool {
        false
    }
}

/// Numeric comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

impl Cmp {
    fn holds(self, a: i64, b: i64) -> bool {
        match self {
            Cmp::Eq => a == b,
            Cmp::Ne => a != b,
            Cmp::Gt => a > b,
            Cmp::Ge => a >= b,
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
        }
    }
}

/// A term's pattern.
#[derive(Debug, Clone)]
pub enum Pattern {
    Glob(String),
    Regex(Arc<Regex>),
    Num(Cmp, i64),
    Relative(String),
}

/// Which value a term reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Field(String),
    Var(String),
    Env(String),
}

/// A parsed expression.
#[derive(Debug, Clone)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    State(String),
    Term(Source, Pattern),
}

#[derive(Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Word(String),
}

fn tokens(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            out.push(Tok::LParen);
            i += 1;
        } else if c == ')' {
            out.push(Tok::RParen);
            i += 1;
        } else {
            let mut w = String::new();
            let mut quoted = false;
            while i < b.len() && (quoted || !(b[i].is_whitespace() || b[i] == '(' || b[i] == ')')) {
                if b[i] == '\\' && quoted && i + 1 < b.len() {
                    w.push(b[i]);
                    i += 1;
                } else if b[i] == '"' {
                    quoted = !quoted;
                }
                w.push(b[i]);
                i += 1;
            }
            if quoted {
                return Err("unterminated quote".into());
            }
            out.push(Tok::Word(w));
        }
    }
    Ok(out)
}

/// Remove one level of `"…"` quoting (with `\"` and `\\` escapes) wherever it appears.
fn unquote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut quoted = false;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                if let Some(n) = it.next() {
                    out.push(n);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn duration_ms(s: &str) -> Option<i64> {
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000)
    } else {
        (s, 1)
    };
    num.parse::<i64>().ok()?.checked_mul(mult)
}

fn numeric(field: &str, pat: &str) -> Result<Pattern, String> {
    let (cmp, rest) = if let Some(r) = pat.strip_prefix(">=") {
        (Cmp::Ge, r)
    } else if let Some(r) = pat.strip_prefix("<=") {
        (Cmp::Le, r)
    } else if let Some(r) = pat.strip_prefix('>') {
        (Cmp::Gt, r)
    } else if let Some(r) = pat.strip_prefix('<') {
        (Cmp::Lt, r)
    } else if let Some(r) = pat.strip_prefix('!') {
        (Cmp::Ne, r)
    } else if let Some(r) = pat.strip_prefix('=') {
        (Cmp::Eq, r)
    } else {
        (Cmp::Eq, pat)
    };
    let n = if is_duration(field) {
        duration_ms(rest)
    } else {
        rest.parse::<i64>().ok()
    };
    n.map(|n| Pattern::Num(cmp, n))
        .ok_or_else(|| format!("`{field}` needs a number, not `{pat}`"))
}

fn regex(p: &str) -> Result<Pattern, String> {
    if p.is_empty() {
        return Err("empty pattern".into());
    }
    if p.len() > MAX_REGEX_BYTES {
        return Err(format!("pattern is longer than {MAX_REGEX_BYTES} bytes"));
    }
    Regex::builder()
        .syntax(regex_automata::util::syntax::Config::new().nest_limit(32))
        .configure(Regex::config().nfa_size_limit(Some(1 << 20)))
        .build(p)
        .map(|r| Pattern::Regex(Arc::new(r)))
        .map_err(|e| {
            let m = e.to_string();
            m.lines().next().unwrap_or("invalid pattern").to_string()
        })
}

fn pattern(field: &str, raw: &str) -> Result<Pattern, String> {
    let p = unquote(raw);
    if p.is_empty() {
        return Err(format!("empty pattern for `{field}`"));
    }
    if let Some(r) = p.strip_prefix('~') {
        return regex(r);
    }
    if field == "id" {
        if RELATIVE.contains(&p.as_str()) {
            return Ok(Pattern::Relative(p));
        }
        if let Ok(n) = p.parse::<i64>() {
            return Ok(Pattern::Num(Cmp::Eq, n));
        }
    }
    if is_numeric(field) {
        return numeric(field, &p);
    }
    Ok(Pattern::Glob(p))
}

fn term(word: &str) -> Result<Expr, String> {
    if MATCH_BARE.contains(&word) {
        return Ok(Expr::State(word.to_string()));
    }
    let (field, rest) = word
        .split_once(':')
        .ok_or_else(|| format!("`{word}` is not `field:pattern`"))?;
    if field == "var" || field == "env" {
        let (name, pat) = rest
            .split_once(':')
            .ok_or_else(|| format!("`{field}:` needs `{field}:<name>:pattern`"))?;
        if name.is_empty() || pat.is_empty() {
            return Err(format!("empty name or pattern for `{field}`"));
        }
        let src = if field == "var" {
            Source::Var(name.to_string())
        } else {
            Source::Env(name.to_string())
        };
        return Ok(Expr::Term(src, pattern(field, pat)?));
    }
    if !MATCH_FIELDS.contains(&field) && event_field(field).is_none() {
        return Err(match crate::diag::suggest(field, MATCH_FIELDS) {
            Some(s) => format!("unknown match field `{field}` (did you mean `{s}`?)"),
            None => format!("unknown match field `{field}`"),
        });
    }
    if rest.is_empty() {
        return Err(format!("empty pattern for `{field}`"));
    }
    if field == "state" {
        let w = unquote(rest);
        // `state` is both the pane-state field and `progress-changed`'s field; the word
        // sets are disjoint, so a progress word matches the event field (spec gap,
        // reported).
        return if MATCH_BARE.contains(&w.as_str()) {
            Ok(Expr::State(w))
        } else if PROGRESS_STATES.contains(&w.as_str()) {
            Ok(Expr::Term(Source::Field("state".into()), Pattern::Glob(w)))
        } else {
            Err(format!("unknown state `{w}`"))
        };
    }
    Ok(Expr::Term(
        Source::Field(field.to_string()),
        pattern(field, rest)?,
    ))
}

struct P<'a> {
    t: &'a [Tok],
    i: usize,
}

impl P<'_> {
    fn word_is(&self, w: &str) -> bool {
        matches!(self.t.get(self.i), Some(Tok::Word(x)) if x == w)
    }

    fn or(&mut self, depth: usize) -> Result<Expr, String> {
        let mut e = self.and(depth)?;
        while self.word_is("or") {
            self.i += 1;
            e = Expr::Or(Box::new(e), Box::new(self.and(depth)?));
        }
        Ok(e)
    }

    fn and(&mut self, depth: usize) -> Result<Expr, String> {
        let mut e = self.unary(depth)?;
        while self.word_is("and") {
            self.i += 1;
            e = Expr::And(Box::new(e), Box::new(self.unary(depth)?));
        }
        Ok(e)
    }

    fn unary(&mut self, depth: usize) -> Result<Expr, String> {
        if depth >= MATCH_MAX_DEPTH {
            return Err(format!(
                "match expression nests deeper than {MATCH_MAX_DEPTH}"
            ));
        }
        match self.t.get(self.i) {
            Some(Tok::Word(w)) if w == "not" => {
                self.i += 1;
                Ok(Expr::Not(Box::new(self.unary(depth + 1)?)))
            }
            Some(Tok::LParen) => {
                self.i += 1;
                let e = self.or(depth + 1)?;
                if self.t.get(self.i) == Some(&Tok::RParen) {
                    self.i += 1;
                    Ok(e)
                } else {
                    Err("missing `)`".into())
                }
            }
            Some(Tok::Word(w)) if w == "and" || w == "or" => {
                Err(format!("`{w}` needs a term before it"))
            }
            Some(Tok::Word(w)) => {
                let e = term(w)?;
                self.i += 1;
                Ok(e)
            }
            Some(Tok::RParen) => Err("unexpected `)`".into()),
            None => Err("expression ends early".into()),
        }
    }
}

impl Expr {
    /// Parse an expression.
    pub fn parse(s: &str) -> Result<Expr, String> {
        if s.len() > MAX_EXPR_BYTES {
            return Err(format!(
                "match expression is longer than {MAX_EXPR_BYTES} bytes"
            ));
        }
        let toks = tokens(s)?;
        if toks.is_empty() {
            return Err("empty match expression".into());
        }
        let mut p = P { t: &toks, i: 0 };
        let e = p.or(0)?;
        if p.i != toks.len() {
            return Err("unexpected trailing input in match expression".into());
        }
        Ok(e)
    }

    /// Evaluate against `f`.
    pub fn eval(&self, f: &dyn Fields) -> bool {
        match self {
            Expr::And(a, b) => a.eval(f) && b.eval(f),
            Expr::Or(a, b) => a.eval(f) || b.eval(f),
            Expr::Not(a) => !a.eval(f),
            Expr::State(w) => f.state(w),
            Expr::Term(src, pat) => {
                if let Pattern::Relative(t) = pat {
                    return f.relative(t);
                }
                if let (Source::Field(name), Pattern::Num(c, n)) = (src, pat) {
                    return f.number(name).is_some_and(|v| c.holds(v, *n));
                }
                let v = match src {
                    Source::Field(n) => f.text(n),
                    Source::Var(n) => f.var(n),
                    Source::Env(n) => f.env(n),
                };
                let Some(v) = v else { return false };
                match pat {
                    Pattern::Glob(g) => glob(g, &v),
                    Pattern::Regex(r) => r.is_match(v.as_ref()),
                    Pattern::Num(c, n) => v.trim().parse().is_ok_and(|x: i64| c.holds(x, *n)),
                    Pattern::Relative(_) => false,
                }
            }
        }
    }

    /// The fields this expression reads (for deciding which events can change its value).
    pub fn fields(&self, out: &mut Vec<String>) {
        match self {
            Expr::And(a, b) | Expr::Or(a, b) => {
                a.fields(out);
                b.fields(out);
            }
            Expr::Not(a) => a.fields(out),
            Expr::State(w) => out.push(w.clone()),
            Expr::Term(Source::Field(n), _) => out.push(n.clone()),
            Expr::Term(Source::Var(n), _) => out.push(format!("var:{n}")),
            Expr::Term(Source::Env(n), _) => out.push(format!("env:{n}")),
        }
    }
}

/// Anchored glob: `*` any run (including empty), `?` one character, `\x` a literal `x`.
/// Linear in practice: the classic single-backtrack-point algorithm, O(n·m) worst case.
pub fn glob(pat: &str, s: &str) -> bool {
    // Pre-split the pattern into items so escapes are resolved once.
    #[derive(Clone, Copy, PartialEq)]
    enum It {
        Star,
        One,
        Lit(char),
    }
    let mut p: Vec<It> = Vec::with_capacity(pat.len());
    let mut it = pat.chars();
    while let Some(c) = it.next() {
        p.push(match c {
            '*' => It::Star,
            '?' => It::One,
            '\\' => It::Lit(it.next().unwrap_or('\\')),
            c => It::Lit(c),
        });
    }
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some(It::Star) => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(It::One) => {
                pi += 1;
                ti += 1;
            }
            Some(It::Lit(c)) if *c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|x| *x == It::Star)
}

/// True when `pat` has a glob metacharacter (an exact pattern otherwise).
pub fn is_wild(pat: &str) -> bool {
    pat.contains(['*', '?'])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct F {
        text: HashMap<&'static str, &'static str>,
        states: Vec<&'static str>,
        vars: HashMap<&'static str, &'static str>,
        rel: Vec<&'static str>,
    }

    impl Fields for F {
        fn text(&self, field: &str) -> Option<Cow<'_, str>> {
            self.text.get(field).map(|s| Cow::Borrowed(*s))
        }
        fn state(&self, w: &str) -> bool {
            self.states.contains(&w)
        }
        fn var(&self, n: &str) -> Option<Cow<'_, str>> {
            self.vars.get(n).map(|s| Cow::Borrowed(*s))
        }
        fn relative(&self, t: &str) -> bool {
            self.rel.contains(&t)
        }
    }

    fn f() -> F {
        F {
            text: HashMap::from([
                ("title", "vim main.rs"),
                ("cwd", "/home/u/src"),
                ("host", "build-07.example"),
                ("status", "2"),
                ("duration", "12500"),
                ("pid", "4242"),
            ]),
            states: vec!["running"],
            vars: HashMap::from([("env", "prod")]),
            rel: vec!["self"],
        }
    }

    fn yes(s: &str) -> bool {
        Expr::parse(s).unwrap().eval(&f())
    }

    #[test]
    fn globs_are_anchored() {
        assert!(glob("vim*", "vim main.rs"));
        assert!(!glob("main*", "vim main.rs"));
        assert!(glob("*main*", "vim main.rs"));
        assert!(glob("v?m *.rs", "vim main.rs"));
        assert!(glob("a\\*b", "a*b"));
        assert!(!glob("a\\*b", "axb"));
        assert!(glob("", ""));
        assert!(!glob("", "x"));
        assert!(glob("*", ""));
        // Pathological pattern stays fast.
        let s = "a".repeat(4000);
        let p = format!("{}b", "*a".repeat(200));
        assert!(!glob(&p, &s));
    }

    #[test]
    fn terms_and_operators() {
        assert!(yes("title:vim*"));
        assert!(yes("title:~main\\.rs$"));
        assert!(!yes("title:main*"));
        assert!(yes("running"));
        assert!(yes("state:running"));
        assert!(!yes("focused"));
        assert!(yes("not focused and running"));
        assert!(yes("focused or title:vim*"));
        // `and` binds tighter than `or`.
        assert!(yes("title:x or running and not focused"));
        assert!(!yes("(title:x or running) and focused"));
        assert!(yes("var:env:prod and host:build-*"));
        assert!(!yes("var:missing:*"));
        assert!(yes("not var:missing:*"));
        assert!(yes("id:self"));
        assert!(!yes("id:last"));
        assert!(yes(r#"cwd:"/home/u/*""#));
    }

    #[test]
    fn numeric_comparisons() {
        assert!(yes("status:!0"));
        assert!(yes("status:2"));
        assert!(!yes("status:0"));
        assert!(yes("duration:>10s"));
        assert!(!yes("duration:>13s"));
        assert!(yes("duration:<=12500ms"));
        assert!(yes("duration:>=1m or pid:>4000"));
        assert!(Expr::parse("status:abc").is_err());
        assert!(Expr::parse("duration:>10x").is_err());
    }

    #[test]
    fn event_fields_are_match_fields() {
        let mut g = f();
        g.text.insert("name", "go");
        g.text.insert("percent", "40");
        g.text.insert("state", "error");
        let ok = |e: &str| Expr::parse(e).unwrap().eval(&g);
        assert!(ok("name:go"));
        assert!(ok("percent:>=40 and percent:<50"));
        assert!(
            Expr::parse("percent:lots").is_err(),
            "integer event fields are numeric"
        );
        // `state:` takes a pane state or a `progress-changed` state.
        assert!(ok("state:error"));
        assert!(!ok("state:paused"));
        assert!(ok("state:running"));
        assert!(Expr::parse("body:*done*").is_ok());
        assert!(Expr::parse("nmae:go").is_err());
    }

    #[test]
    fn errors_are_reported() {
        for bad in [
            "",
            "titel:x",
            "title:",
            "(title:x",
            "title:x)",
            "and title:x",
            "title:x or",
            "nonsense",
            "title:~(",
            "state:sleepy",
            "var:x",
        ] {
            assert!(Expr::parse(bad).is_err(), "{bad}");
        }
        let long = "title:x ".repeat(MAX_EXPR_BYTES);
        assert!(Expr::parse(&long).is_err());
    }

    #[test]
    fn missing_fields_never_match() {
        let e = Expr::parse("process:ssh").unwrap();
        assert!(!e.eval(&f()));
        assert!(Expr::parse("not process:ssh").unwrap().eval(&f()));
    }
}
