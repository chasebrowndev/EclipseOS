// SPDX-License-Identifier: AGPL-3.0-only
//! A small KDL (v2) reader for the fixture and conformance files.
//!
//! Supported: nodes, `;`/newline separators, `{}` children, positional values, `key=value`
//! properties, quoted strings (`\n \r \t \\ \" \b \f \s \u{..}`), raw strings (`#"..."#`),
//! integers, `#true`/`#false`/`#null`, `//` and `/* */` comments. Not supported (rejected
//! with an error, never misread): type annotations, slashdash, line continuations,
//! multi-line strings, floats. Files stay valid KDL so `xtask` may swap in a full parser.

use std::fmt;

/// A KDL value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// String (quoted or raw).
    Str(String),
    /// Integer.
    Int(i64),
    /// `#true` / `#false`.
    Bool(bool),
    /// `#null`.
    Null,
}

impl Value {
    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    /// The integer, if this is one.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    /// The bool, if this is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// A node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Node name.
    pub name: String,
    /// Positional values.
    pub args: Vec<Value>,
    /// Properties in source order.
    pub props: Vec<(String, Value)>,
    /// Children.
    pub children: Vec<Node>,
    /// 1-based source line of the node name.
    pub line: usize,
}

impl Node {
    /// Property by key.
    #[must_use]
    pub fn prop(&self, key: &str) -> Option<&Value> {
        self.props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    /// First child with this name.
    #[must_use]
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
    /// All children with this name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |c| c.name == name)
    }
}

/// A parse error with position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line.
    pub line: usize,
    /// 1-based column.
    pub col: usize,
    /// What went wrong.
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

impl std::error::Error for ParseError {}

struct P<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
    line: usize,
    line_start: usize,
}

type R<T> = Result<T, ParseError>;

/// Parse a document into top-level nodes.
///
/// # Errors
/// Returns the first syntax error with its line and column.
pub fn parse(src: &str) -> R<Vec<Node>> {
    let mut p = P {
        s: src.as_bytes(),
        src,
        i: 0,
        line: 1,
        line_start: 0,
    };
    let nodes = p.nodes(false)?;
    Ok(nodes)
}

fn is_ident_byte(b: u8) -> bool {
    !(b.is_ascii_whitespace()
        || matches!(
            b,
            b'(' | b')' | b'{' | b'}' | b'[' | b']' | b'/' | b'\\' | b'"' | b'#' | b';' | b'='
        ))
}

impl P<'_> {
    fn err<T>(&self, msg: impl Into<String>) -> R<T> {
        Err(ParseError {
            line: self.line,
            col: self.i - self.line_start + 1,
            msg: msg.into(),
        })
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn bump(&mut self) {
        if self.s[self.i] == b'\n' {
            self.line += 1;
            self.line_start = self.i + 1;
        }
        self.i += 1;
    }

    /// Skip spaces, tabs and comments on one line. Returns true if a newline or EOF follows.
    fn skip_inline(&mut self) -> R<()> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r') => self.i += 1,
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'/') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.i += 1;
                    }
                }
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    loop {
                        match self.peek() {
                            None => return self.err("unterminated comment"),
                            Some(b'*') if self.s.get(self.i + 1) == Some(&b'/') => {
                                self.i += 2;
                                break;
                            }
                            Some(_) => self.bump(),
                        }
                    }
                }
                Some(b'\\') => return self.err("line continuation is not supported"),
                _ => return Ok(()),
            }
        }
    }

    fn skip_ws_and_seps(&mut self) -> R<()> {
        loop {
            self.skip_inline()?;
            match self.peek() {
                Some(b'\n' | b';') => self.bump(),
                _ => return Ok(()),
            }
        }
    }

    fn nodes(&mut self, nested: bool) -> R<Vec<Node>> {
        let mut out = Vec::new();
        loop {
            self.skip_ws_and_seps()?;
            match self.peek() {
                None => {
                    if nested {
                        return self.err("missing `}`");
                    }
                    return Ok(out);
                }
                Some(b'}') => {
                    if !nested {
                        return self.err("unexpected `}`");
                    }
                    self.i += 1;
                    return Ok(out);
                }
                Some(_) => out.push(self.node()?),
            }
        }
    }

    fn node(&mut self) -> R<Node> {
        if self.peek() == Some(b'/') && self.s.get(self.i + 1) == Some(&b'-') {
            return self.err("slashdash is not supported");
        }
        let line = self.line;
        let name = match self.peek() {
            Some(b'"') => self.quoted()?,
            Some(b'#') => return self.err("a node name cannot be a raw string here"),
            _ => self.ident()?,
        };
        let mut node = Node {
            name,
            args: Vec::new(),
            props: Vec::new(),
            children: Vec::new(),
            line,
        };
        loop {
            self.skip_inline()?;
            match self.peek() {
                None | Some(b'\n' | b';') => return Ok(node),
                Some(b'}') => return Ok(node),
                Some(b'{') => {
                    self.i += 1;
                    node.children = self.nodes(true)?;
                    // After a child block only separators may follow.
                    self.skip_inline()?;
                    match self.peek() {
                        None | Some(b'\n' | b';' | b'}') => return Ok(node),
                        _ => return self.err("unexpected content after `}`"),
                    }
                }
                Some(b'(') => return self.err("type annotations are not supported"),
                Some(_) => {
                    // Property (ident `=` value) or positional value.
                    let save = (self.i, self.line, self.line_start);
                    if let Some(b) = self.peek()
                        && is_ident_byte(b)
                        && !b.is_ascii_digit()
                        && b != b'-'
                        && b != b'+'
                    {
                        let key = self.ident()?;
                        if self.peek() == Some(b'=') {
                            self.i += 1;
                            let v = self.value()?;
                            node.props.push((key, v));
                            continue;
                        }
                        (self.i, self.line, self.line_start) = save;
                    }
                    let v = self.value()?;
                    node.args.push(v);
                }
            }
        }
    }

    fn ident(&mut self) -> R<String> {
        let start = self.i;
        while let Some(b) = self.peek() {
            if is_ident_byte(b) {
                self.i += 1;
            } else {
                break;
            }
        }
        if start == self.i {
            return self.err("expected a name");
        }
        Ok(self.src[start..self.i].to_string())
    }

    fn value(&mut self) -> R<Value> {
        match self.peek() {
            Some(b'"') => {
                if self.s[self.i..].starts_with(b"\"\"\"") {
                    return self.err("multi-line strings are not supported");
                }
                Ok(Value::Str(self.quoted()?))
            }
            Some(b'#') => {
                let rest = &self.s[self.i..];
                if rest.starts_with(b"#true") {
                    self.i += 5;
                    Ok(Value::Bool(true))
                } else if rest.starts_with(b"#false") {
                    self.i += 6;
                    Ok(Value::Bool(false))
                } else if rest.starts_with(b"#null") {
                    self.i += 5;
                    Ok(Value::Null)
                } else {
                    Ok(Value::Str(self.raw()?))
                }
            }
            Some(b) if b.is_ascii_digit() || b == b'-' || b == b'+' => {
                let start = self.i;
                self.i += 1;
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() || c == b'_' {
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                let t: String = self.src[start..self.i]
                    .chars()
                    .filter(|&c| c != '_')
                    .collect();
                match t.parse::<i64>() {
                    Ok(v) => Ok(Value::Int(v)),
                    Err(_) => self.err(format!("bad integer `{t}`")),
                }
            }
            _ => self.err("expected a value (strings must be quoted)"),
        }
    }

    fn raw(&mut self) -> R<String> {
        let mut hashes = 0;
        while self.peek() == Some(b'#') {
            hashes += 1;
            self.i += 1;
        }
        if self.peek() != Some(b'"') {
            return self.err("expected `\"` after `#`");
        }
        self.i += 1;
        let start = self.i;
        loop {
            match self.peek() {
                None => return self.err("unterminated raw string"),
                Some(b'"') => {
                    let mut n = 0;
                    while n < hashes && self.s.get(self.i + 1 + n) == Some(&b'#') {
                        n += 1;
                    }
                    if n == hashes {
                        let text = self.src[start..self.i].to_string();
                        self.i += 1 + hashes;
                        return Ok(text);
                    }
                    self.i += 1;
                }
                Some(_) => self.bump(),
            }
        }
    }

    fn quoted(&mut self) -> R<String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let Some(c) = self.src[self.i..].chars().next() else {
                return self.err("unterminated string");
            };
            match c {
                '"' => {
                    self.i += 1;
                    return Ok(out);
                }
                '\n' => return self.err("newline in a quoted string"),
                '\\' => {
                    self.i += 1;
                    let Some(e) = self.peek() else {
                        return self.err("dangling `\\`");
                    };
                    self.i += 1;
                    match e {
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'\\' => out.push('\\'),
                        b'"' => out.push('"'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b's' => out.push(' '),
                        b'u' => {
                            if self.peek() != Some(b'{') {
                                return self.err("expected `{` after `\\u`");
                            }
                            self.i += 1;
                            let start = self.i;
                            while self.peek().is_some_and(|b| b != b'}') {
                                self.i += 1;
                            }
                            let hex = &self.src[start..self.i];
                            self.i += 1;
                            let Some(ch) =
                                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                            else {
                                return self.err(format!("bad \\u{{{hex}}}"));
                            };
                            out.push(ch);
                        }
                        other => return self.err(format!("unknown escape `\\{}`", other as char)),
                    }
                }
                _ => {
                    out.push(c);
                    self.i += c.len_utf8();
                }
            }
        }
    }
}

/// Quote a string as a KDL quoted string.
#[must_use]
pub fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f || (0x80..0xa0).contains(&(c as u32)) => {
                o.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Emit a raw string wide enough that the content cannot terminate it.
#[must_use]
pub fn raw(s: &str) -> String {
    let mut hashes = 1;
    loop {
        let close = format!("\"{}", "#".repeat(hashes));
        if !s.contains(&close) {
            break;
        }
        hashes += 1;
    }
    let h = "#".repeat(hashes);
    format!("{h}\"{s}\"{h}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_args_props_children() {
        let n = parse("a 1 \"x\" k=#true { b; c \"d\" }\n// c\ne").unwrap();
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].args, vec![Value::Int(1), Value::Str("x".into())]);
        assert_eq!(n[0].prop("k"), Some(&Value::Bool(true)));
        assert_eq!(n[0].children.len(), 2);
        assert_eq!(n[0].children[1].args[0].as_str(), Some("d"));
        assert_eq!(n[1].name, "e");
        assert_eq!(n[1].line, 3);
    }

    #[test]
    fn raw_strings_keep_backslashes() {
        let n = parse("feed #\"\\e[1m\\\"x\"#").unwrap();
        assert_eq!(n[0].args[0].as_str(), Some("\\e[1m\\\"x"));
        let n = parse("feed ##\"a\"#b\"##").unwrap();
        assert_eq!(n[0].args[0].as_str(), Some("a\"#b"));
    }

    #[test]
    fn quoted_escapes() {
        let n = parse(r#"x "a\n\u{1b}\"\\""#).unwrap();
        assert_eq!(n[0].args[0].as_str(), Some("a\n\u{1b}\"\\"));
    }

    #[test]
    fn quote_and_raw_round_trip() {
        for s in ["plain", "a\"b\\c\n\u{1b}\u{7f}é", ""] {
            let n = parse(&format!("x {}", quote(s))).unwrap();
            assert_eq!(n[0].args[0].as_str(), Some(s));
        }
        for s in ["\\e[m", "q\"#r", "a\"##b\"#"] {
            let n = parse(&format!("x {}", raw(s))).unwrap();
            assert_eq!(n[0].args[0].as_str(), Some(s));
        }
    }

    #[test]
    fn errors_have_positions() {
        let e = parse("a\nb \"unterminated").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(parse("a {").is_err());
        assert!(parse("}").is_err());
        assert!(parse("a bare").is_err());
        assert!(parse("/- a").is_err());
        assert!(parse("a (t)1").is_err());
        assert!(parse("a \"\"\"x\"\"\"").is_err());
    }

    #[test]
    fn quoted_node_names_and_negative_ints() {
        let n = parse("\"?25\" -3").unwrap();
        assert_eq!(n[0].name, "?25");
        assert_eq!(n[0].args[0].as_int(), Some(-3));
    }
}
