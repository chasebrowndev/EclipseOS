// SPDX-License-Identifier: AGPL-3.0-only
//! Templates (T-11 §8): tab and window titles, badges, status segments, notification text.
//!
//! ```text
//! template := (literal | "{{" | "}}" | "{" item "}")*
//! item     := var ("|" filter)* | "if" SP cond | "else" | "end"
//! var      := name ("." name)*            e.g. title, cwd, var.branch, env.USER
//! filter   := "basename" | "truncate:" N | "default:" STRING | "upper" | "lower"
//! cond     := var ("==" | "!=") STRING | var | match-expression (T-11 §7)
//! ```
//!
//! A bare `{if name}` is true when the value is non-empty. Values are read through
//! [`Fields`] (`var.<name>` and `env.<name>` through its `var`/`env`). Substituted values
//! lose their control characters (program-set text, T-18 §5), and the output is capped at
//! [`MAX_OUTPUT_BYTES`]. No loops, no user functions.

use std::borrow::Cow;

use crate::expr::{Expr, Fields};

/// Longest template accepted.
pub const MAX_TEMPLATE_BYTES: usize = 4096;
/// Longest rendered output; longer output is cut at a character boundary.
pub const MAX_OUTPUT_BYTES: usize = 4096;
/// `{if}` nesting bound.
pub const MAX_IF_DEPTH: usize = 16;
/// Largest `truncate:` argument.
pub const MAX_TRUNCATE: usize = 4096;

/// One filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    Basename,
    Truncate(usize),
    Default(String),
    Upper,
    Lower,
}

/// A condition of `{if …}`.
#[derive(Debug, Clone)]
pub enum Cond {
    Cmp { name: String, eq: bool, lit: String },
    Truthy(String),
    Expr(Expr),
}

#[derive(Debug, Clone)]
enum Node {
    Lit(String),
    Var {
        name: String,
        filters: Vec<Filter>,
    },
    If {
        cond: Cond,
        then: Vec<Node>,
        els: Vec<Node>,
    },
}

/// A parsed template.
#[derive(Debug, Clone)]
pub struct Template {
    nodes: Vec<Node>,
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

/// Parse a `"…"` literal (with `\"` and `\\`); the whole of `s` must be the literal.
fn literal(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => out.push(it.next()?),
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// Split at the first `op` outside quotes.
fn split_op<'a>(s: &'a str, op: &str) -> Option<(&'a str, &'a str)> {
    let mut quoted = false;
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            '\\' if quoted => esc = true,
            '"' => quoted = !quoted,
            _ if !quoted && s[i..].starts_with(op) => return Some((&s[..i], &s[i + op.len()..])),
            _ => {}
        }
    }
    None
}

fn cond(s: &str) -> Result<Cond, String> {
    let s = s.trim();
    for (op, eq) in [("==", true), ("!=", false)] {
        if let Some((l, r)) = split_op(s, op) {
            let name = l.trim();
            if !is_name(name) {
                return Err(format!("`{name}` is not a field name"));
            }
            let lit = literal(r.trim()).ok_or("the right side of a comparison is a \"string\"")?;
            return Ok(Cond::Cmp {
                name: name.to_string(),
                eq,
                lit,
            });
        }
    }
    if is_name(s) && !crate::lang::MATCH_BARE.contains(&s) {
        return Ok(Cond::Truthy(s.to_string()));
    }
    Expr::parse(s).map(Cond::Expr)
}

fn filter(s: &str) -> Result<Filter, String> {
    let s = s.trim();
    let (name, arg) = match s.split_once(':') {
        Some((n, a)) => (n.trim(), Some(a.trim())),
        None => (s, None),
    };
    match (name, arg) {
        ("basename", None) => Ok(Filter::Basename),
        ("upper", None) => Ok(Filter::Upper),
        ("lower", None) => Ok(Filter::Lower),
        ("truncate", Some(a)) => match a.parse::<usize>() {
            Ok(n) if (1..=MAX_TRUNCATE).contains(&n) => Ok(Filter::Truncate(n)),
            _ => Err(format!("truncate takes 1..={MAX_TRUNCATE}")),
        },
        ("default", Some(a)) => literal(a)
            .map(Filter::Default)
            .ok_or_else(|| "default takes a \"string\"".to_string()),
        _ => Err(format!("unknown filter `{s}`")),
    }
}

/// Split `item` at `|` outside quotes.
fn pipes(item: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = item;
    while let Some((a, b)) = split_op(rest, "|") {
        out.push(a);
        rest = b;
    }
    out.push(rest);
    out
}

impl Template {
    /// Parse a template.
    pub fn parse(s: &str) -> Result<Template, String> {
        if s.len() > MAX_TEMPLATE_BYTES {
            return Err(format!(
                "template is longer than {MAX_TEMPLATE_BYTES} bytes"
            ));
        }
        // Stack of open blocks: (nodes before the `if`, cond, then, in else?).
        let mut stack: Vec<(Vec<Node>, Cond, Vec<Node>, bool)> = Vec::new();
        let mut cur: Vec<Node> = Vec::new();
        let mut lit = String::new();
        let b: Vec<char> = s.chars().collect();
        let mut i = 0;
        let flush = |lit: &mut String, cur: &mut Vec<Node>| {
            if !lit.is_empty() {
                cur.push(Node::Lit(std::mem::take(lit)));
            }
        };
        while i < b.len() {
            match b[i] {
                '{' if b.get(i + 1) == Some(&'{') => {
                    lit.push('{');
                    i += 2;
                }
                '}' if b.get(i + 1) == Some(&'}') => {
                    lit.push('}');
                    i += 2;
                }
                '{' => {
                    let mut j = i + 1;
                    let mut quoted = false;
                    while j < b.len() && (quoted || b[j] != '}') {
                        if b[j] == '\\' && quoted {
                            j += 2;
                            continue;
                        }
                        if b[j] == '"' {
                            quoted = !quoted;
                        }
                        j += 1;
                    }
                    if j >= b.len() {
                        return Err("unclosed `{` in template".into());
                    }
                    let inner: String = b[i + 1..j].iter().collect();
                    let inner = inner.trim();
                    if inner.is_empty() {
                        return Err("empty `{}` in template".into());
                    }
                    flush(&mut lit, &mut cur);
                    if let Some(c) = inner.strip_prefix("if ").or((inner == "if").then_some("")) {
                        if stack.len() >= MAX_IF_DEPTH {
                            return Err(format!("`{{if}}` nests deeper than {MAX_IF_DEPTH}"));
                        }
                        let c = cond(c).map_err(|e| format!("bad `{{if}}`: {e}"))?;
                        stack.push((std::mem::take(&mut cur), c, Vec::new(), false));
                    } else if inner == "else" {
                        match stack.last_mut() {
                            Some(top) if !top.3 => {
                                top.2 = std::mem::take(&mut cur);
                                top.3 = true;
                            }
                            _ => return Err("`{else}` without a matching `{if}`".into()),
                        }
                    } else if inner == "end" {
                        let Some((before, c, then, in_else)) = stack.pop() else {
                            return Err("`{end}` without a matching `{if}`".into());
                        };
                        let (then, els) = if in_else {
                            (then, std::mem::take(&mut cur))
                        } else {
                            (std::mem::take(&mut cur), Vec::new())
                        };
                        cur = before;
                        cur.push(Node::If { cond: c, then, els });
                    } else {
                        let parts = pipes(inner);
                        let name = parts[0].trim();
                        if !is_name(name) {
                            return Err(format!("`{name}` is not a field name"));
                        }
                        let filters = parts[1..]
                            .iter()
                            .map(|f| filter(f))
                            .collect::<Result<Vec<_>, _>>()?;
                        cur.push(Node::Var {
                            name: name.to_string(),
                            filters,
                        });
                    }
                    i = j + 1;
                }
                '}' => return Err("stray `}` in template".into()),
                c => {
                    lit.push(c);
                    i += 1;
                }
            }
        }
        if !stack.is_empty() {
            return Err("`{if}` without `{end}`".into());
        }
        flush(&mut lit, &mut cur);
        Ok(Template { nodes: cur })
    }

    /// Render against `f`.
    pub fn render(&self, f: &dyn Fields) -> String {
        let mut out = String::new();
        render(&self.nodes, f, &mut out);
        if out.len() > MAX_OUTPUT_BYTES {
            let mut cut = MAX_OUTPUT_BYTES;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
        }
        out
    }
}

/// Look a template name up: `var.x`, `env.X`, or a field.
fn value<'a>(f: &'a dyn Fields, name: &str) -> Option<Cow<'a, str>> {
    if let Some(v) = name.strip_prefix("var.") {
        f.var(v)
    } else if let Some(e) = name.strip_prefix("env.") {
        f.env(e)
    } else {
        f.text(name)
    }
}

fn holds(c: &Cond, f: &dyn Fields) -> bool {
    match c {
        Cond::Cmp { name, eq, lit } => {
            let v = value(f, name);
            (v.as_deref() == Some(lit.as_str())) == *eq
        }
        Cond::Truthy(name) => value(f, name).is_some_and(|v| !v.is_empty()),
        Cond::Expr(e) => e.eval(f),
    }
}

fn render(nodes: &[Node], f: &dyn Fields, out: &mut String) {
    for n in nodes {
        if out.len() > MAX_OUTPUT_BYTES {
            return;
        }
        match n {
            Node::Lit(s) => out.push_str(s),
            Node::Var { name, filters } => {
                let mut v: String = value(f, name)
                    .map(|v| {
                        v.chars()
                            .filter(|c| !c.is_control())
                            .take(MAX_OUTPUT_BYTES)
                            .collect()
                    })
                    .unwrap_or_default();
                for fl in filters {
                    v = match fl {
                        Filter::Basename => v
                            .trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .filter(|s| !s.is_empty())
                            .map_or_else(|| v.clone(), str::to_string),
                        Filter::Truncate(n) => {
                            if v.chars().count() > *n {
                                let mut t: String = v.chars().take(n.saturating_sub(1)).collect();
                                t.push('…');
                                t
                            } else {
                                v
                            }
                        }
                        Filter::Default(d) if v.is_empty() => d.clone(),
                        Filter::Default(_) => v,
                        Filter::Upper => v.to_uppercase(),
                        Filter::Lower => v.to_lowercase(),
                    };
                }
                out.push_str(&v);
            }
            Node::If { cond, then, els } => {
                if holds(cond, f) {
                    render(then, f, out);
                } else {
                    render(els, f, out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct F(HashMap<&'static str, &'static str>);

    impl Fields for F {
        fn text(&self, field: &str) -> Option<Cow<'_, str>> {
            self.0.get(field).map(|s| Cow::Borrowed(*s))
        }
        fn var(&self, n: &str) -> Option<Cow<'_, str>> {
            self.0
                .get(format!("v:{n}").as_str())
                .map(|s| Cow::Borrowed(*s))
        }
        fn state(&self, w: &str) -> bool {
            w == "running"
        }
    }

    fn r(t: &str) -> String {
        let f = F(HashMap::from([
            ("title", "vim"),
            ("cwd", "/home/u/src/apotheosis/"),
            ("host", "box"),
            ("status", "1"),
            ("evil", "a\x1b]0;x\x07b"),
            ("v:branch", "main"),
        ]));
        Template::parse(t).unwrap().render(&f)
    }

    #[test]
    fn variables_and_filters() {
        assert_eq!(r("{title} - {cwd|basename}"), "vim - apotheosis");
        assert_eq!(r("{title|upper}"), "VIM");
        assert_eq!(r("{cwd|truncate:6}"), "/home…");
        assert_eq!(r(r#"{nothing|default:"(none)"}"#), "(none)");
        assert_eq!(r("[{var.branch}]"), "[main]");
        assert_eq!(r("{{literal}}"), "{literal}");
        assert_eq!(r("{evil}"), "a]0;xb", "controls are stripped");
    }

    #[test]
    fn conditions() {
        assert_eq!(r("{if host}[{host}] {end}{title}"), "[box] vim");
        assert_eq!(r("{if user}u{else}no user{end}"), "no user");
        assert_eq!(r(r#"{if title == "vim"}V{else}x{end}"#), "V");
        assert_eq!(r(r#"{if title != "vim"}V{else}x{end}"#), "x");
        assert_eq!(r("{if status:!0 and running}failed{end}"), "failed");
        assert_eq!(r("{if not running}a{else}{if title:v*}b{end}{end}"), "b");
    }

    #[test]
    fn errors() {
        for bad in [
            "{",
            "}",
            "{}",
            "{if x}",
            "{else}",
            "{end}",
            "{if x}{else}{else}{end}",
            "{title|bogus}",
            "{title|truncate:0}",
            "{title|default:x}",
            "{bad name}",
            "{if titel:x}y{end}",
            r#"{if x == y}a{end}"#,
        ] {
            assert!(Template::parse(bad).is_err(), "{bad}");
        }
        let deep = format!("{}{}", "{if x}".repeat(17), "{end}".repeat(17));
        assert!(Template::parse(&deep).is_err());
    }

    #[test]
    fn output_is_capped() {
        let t = "{title}".repeat(MAX_TEMPLATE_BYTES / 7);
        let f = F(HashMap::from([("title", "€€€€€€€€€€")]));
        let out = Template::parse(&t).unwrap().render(&f);
        assert!(out.len() <= MAX_OUTPUT_BYTES);
    }
}
