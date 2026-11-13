// SPDX-License-Identifier: Apache-2.0
//! The textual action syntax (AUTOMATION-24): used by `apoth do`, the palette input and
//! KDL `do` bodies.
//!
//! ```text
//! script  := call (';' call)*
//! call    := id (positional | name '=' value)* ('--' argv-element*)?
//! value   := bare | '"' quoted '"'
//! ```
//! - `id` is `family.verb` (or a bare closed id such as `launch`).
//! - A bare word is made of letters, digits and `_ . / : @ % + , { } * ~ - ! ?`; anything
//!   else (spaces, `;`, `=`, quotes, empty) is double-quoted.
//! - Quoted escapes: `\" \\ \n \r \t \e \0 \xHH \u{H..}`.
//! - Positional values fill the parameters in declaration order, skipping any given by
//!   name; repeatable and argv parameters are never positional.
//! - After `--` every token is an argv element (bare words may contain `=`).
//! - Total script size is limited by [`MAX_SCRIPT_BYTES`].

use crate::error::{ErrorKind, ParseError};

/// Largest script accepted (matches the socket line limit, T-12 §1).
pub const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
/// Most calls in one script.
pub const MAX_CALLS: usize = 1024;

/// One parsed (but not yet typed) action call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// Action id.
    pub id: String,
    /// Positional values, in order.
    pub positional: Vec<String>,
    /// `name=value` pairs, in order; a name may repeat for repeatable parameters.
    pub named: Vec<(String, String)>,
    /// The argv tail after `--`, in source form; `None` when there was no `--`.
    pub argv: Option<Vec<String>>,
}

impl Call {
    /// An empty call to `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            positional: Vec::new(),
            named: Vec::new(),
            argv: None,
        }
    }

    /// Format as text (single call, no trailing `;`).
    pub fn format(&self) -> String {
        let mut s = self.id.clone();
        for p in &self.positional {
            s.push(' ');
            s.push_str(&quote(p, false));
        }
        for (k, v) in &self.named {
            s.push(' ');
            s.push_str(k);
            s.push('=');
            s.push_str(&quote(v, false));
        }
        if let Some(a) = &self.argv {
            s.push_str(" --");
            for e in a {
                s.push(' ');
                s.push_str(&quote(e, true));
            }
        }
        s
    }
}

fn is_bare(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '_' | '.' | '/' | ':' | '@' | '%' | '+' | ',' | '{' | '}' | '*' | '~' | '-' | '!' | '?'
        )
}

fn is_ident(s: &str) -> bool {
    let mut it = s.chars();
    matches!(it.next(), Some(c) if c.is_ascii_lowercase())
        && it.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && s.starts_with(|c: char| c.is_ascii_lowercase())
}

/// Quote `s` if it is not a bare word. In `argv` mode `=` is allowed bare.
pub fn quote(s: &str, argv: bool) -> String {
    let bare = !s.is_empty() && s != "--" && s.chars().all(|c| is_bare(c) || (argv && c == '='));
    if bare {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x1b' => out.push_str("\\e"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 || c == '\x7f' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
}

enum Tok {
    Pos(String),
    Named(String, String),
    Sep,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.pos += c.len_utf8();
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.bump();
        }
    }

    fn err(&self, msg: &str) -> ParseError {
        ParseError::new(ErrorKind::Syntax, msg).at(self.pos)
    }

    fn bare(&mut self, allow_eq: bool) -> String {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if is_bare(c) || (allow_eq && c == '=')) {
            self.bump();
        }
        self.src[start..self.pos].to_string()
    }

    fn quoted(&mut self) -> Result<String, ParseError> {
        let open = self.pos;
        self.bump();
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(ParseError::new(ErrorKind::Syntax, "unterminated string").at(open));
            };
            self.bump();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let Some(e) = self.peek() else {
                        return Err(self.err("unterminated escape"));
                    };
                    self.bump();
                    match e {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'e' => out.push('\x1b'),
                        '0' => out.push('\0'),
                        'x' => {
                            let h = self.src.get(self.pos..self.pos + 2).unwrap_or("");
                            let v = u8::from_str_radix(h, 16)
                                .ok()
                                .filter(|v| *v < 0x80 && h.len() == 2)
                                .ok_or_else(|| self.err("bad \\x escape (00-7f)"))?;
                            self.pos += 2;
                            out.push(v as char);
                        }
                        'u' => {
                            if self.peek() != Some('{') {
                                return Err(self.err("expected `{` after \\u"));
                            }
                            self.bump();
                            let start = self.pos;
                            while matches!(self.peek(), Some(c) if c.is_ascii_hexdigit()) {
                                self.bump();
                            }
                            let h = &self.src[start..self.pos];
                            if self.peek() != Some('}') || h.is_empty() || h.len() > 6 {
                                return Err(self.err("bad \\u{..} escape"));
                            }
                            self.bump();
                            let ch = u32::from_str_radix(h, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .ok_or_else(|| self.err("bad \\u{..} code point"))?;
                            out.push(ch);
                        }
                        _ => return Err(self.err("unknown escape")),
                    }
                }
                c => out.push(c),
            }
        }
    }

    fn token(&mut self, argv_mode: bool) -> Result<Tok, ParseError> {
        if self.peek() == Some('"') {
            return self.quoted().map(Tok::Pos);
        }
        let head = self.bare(argv_mode);
        match self.peek() {
            Some('=') => {
                if !is_ident(&head) {
                    return Err(
                        self.err("unquoted `=`: quote the value, or name must be kebab-case")
                    );
                }
                self.bump();
                let value = if self.peek() == Some('"') {
                    self.quoted()?
                } else {
                    self.bare(false)
                };
                match self.peek() {
                    None | Some(';') => {}
                    Some(c) if c.is_whitespace() => {}
                    Some(_) => return Err(self.err("unexpected character after value")),
                }
                Ok(Tok::Named(head, value))
            }
            Some('"') => Err(self.err("unexpected quote inside a word")),
            Some(c) if !c.is_whitespace() && c != ';' => Err(self.err("unexpected character")),
            _ => {
                if head.is_empty() {
                    return Err(self.err("empty token"));
                }
                if head == "--" && !argv_mode {
                    Ok(Tok::Sep)
                } else {
                    Ok(Tok::Pos(head))
                }
            }
        }
    }
}

/// Parse a `;`-separated script into untyped calls.
pub fn parse_calls(src: &str) -> Result<Vec<Call>, ParseError> {
    if src.len() > MAX_SCRIPT_BYTES {
        return Err(ParseError::new(
            ErrorKind::Syntax,
            format!("script is {} bytes, limit is {MAX_SCRIPT_BYTES}", src.len()),
        ));
    }
    let mut lx = Lexer { src, pos: 0 };
    let mut calls = Vec::new();
    loop {
        lx.skip_ws();
        match lx.peek() {
            None => break,
            Some(';') => {
                lx.bump();
                continue;
            }
            Some(_) => {}
        }
        if calls.len() >= MAX_CALLS {
            return Err(lx.err("too many actions in one script"));
        }
        let start = lx.pos;
        let id = lx.bare(false);
        if !is_id(&id) {
            return Err(ParseError::new(ErrorKind::Syntax, "expected an action id").at(start));
        }
        let mut call = Call::new(id);
        loop {
            lx.skip_ws();
            match lx.peek() {
                None | Some(';') => break,
                Some(_) => {}
            }
            let in_argv = call.argv.is_some();
            match lx.token(in_argv)? {
                Tok::Sep => call.argv = Some(Vec::new()),
                Tok::Pos(v) => match &mut call.argv {
                    Some(a) => a.push(v),
                    None => call.positional.push(v),
                },
                Tok::Named(k, v) => call.named.push((k, v)),
            }
        }
        calls.push(call);
    }
    Ok(calls)
}

/// Format calls as a script.
pub fn format_calls(calls: &[Call]) -> String {
    calls
        .iter()
        .map(Call::format)
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> Call {
        let mut v = parse_calls(s).unwrap();
        assert_eq!(v.len(), 1, "{s}");
        v.remove(0)
    }

    #[test]
    fn positional_named_and_argv() {
        let c = one(r#"launch tab cwd=home hold=true -- nvim "a b" --x=1 {file}"#);
        assert_eq!(c.id, "launch");
        assert_eq!(c.positional, vec!["tab"]);
        assert_eq!(
            c.named,
            vec![
                ("cwd".into(), "home".into()),
                ("hold".into(), "true".into())
            ]
        );
        assert_eq!(c.argv.unwrap(), vec!["nvim", "a b", "--x=1", "{file}"]);
    }

    #[test]
    fn chaining_and_empty_statements() {
        let v = parse_calls("a.b x ;; c.d ; ").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].id, "c.d");
    }

    #[test]
    fn semicolon_in_quotes_is_data() {
        let c = one(r#"input.send-text "a;b""#);
        assert_eq!(c.positional, vec!["a;b"]);
    }

    #[test]
    fn escapes() {
        let c = one(r#"x "\n\t\e\0\x41\u{1F600}\"\\""#);
        assert_eq!(c.positional[0], "\n\t\x1b\0A\u{1F600}\"\\");
    }

    #[test]
    fn syntax_errors_carry_offsets() {
        for bad in [
            r#"x "unterminated"#,
            r#"x "\q""#,
            r#"x "\x4""#,
            r#"x "\xff""#,
            "x a=\"b\"c",
            "x =y",
            "x A=1",
            "x a\"b\"",
            "1bad",
            "x 'q'",
        ] {
            let e = parse_calls(bad).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Syntax, "{bad}");
            assert!(e.offset.is_some(), "{bad}");
        }
    }

    #[test]
    fn quote_roundtrip() {
        let cases = [
            "", "a", "a b", "a=b", "--", "-x", "a;b", "q\"q", "\\", "\n", "\x01", "é", "{url}",
            "x\u{7f}",
        ];
        for s in cases {
            let q = quote(s, false);
            let c = parse_calls(&format!("x {q}")).unwrap();
            assert_eq!(c[0].positional, vec![s.to_string()], "{s:?} -> {q}");
            let q = quote(s, true);
            let c = parse_calls(&format!("x -- {q}")).unwrap();
            assert_eq!(c[0].argv, Some(vec![s.to_string()]), "{s:?} -> {q}");
        }
    }

    #[test]
    fn format_parse_roundtrip_with_argv() {
        let mut c = Call::new("pipe");
        c.positional.push("selection".into());
        c.named.push(("k".into(), "v w".into()));
        c.argv = Some(vec![
            "sh".into(),
            "-c".into(),
            "echo $1".into(),
            "--".into(),
            "{url}".into(),
        ]);
        let t = c.format();
        assert_eq!(parse_calls(&t).unwrap(), vec![c]);
    }

    #[test]
    fn size_limits() {
        let big = format!("x \"{}\"", "a".repeat(MAX_SCRIPT_BYTES));
        assert!(parse_calls(&big).is_err());
        let many = "a.b;".repeat(MAX_CALLS + 1);
        assert!(parse_calls(&many).is_err());
    }
}
