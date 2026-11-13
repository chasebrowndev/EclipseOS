// SPDX-License-Identifier: AGPL-3.0-only
//! A small KDL-subset reader for fixtures: nodes with string / integer / `#true` / `#false`
//! arguments, `key=value` properties and `{}` children. Strings take `\n \r \t \e \a \b \0
//! \\ \" \xNN \u{..}`; `\xNN` is limited to ASCII (use `feed-hex` for raw bytes).

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Str(String),
    Int(i64),
    Bool(bool),
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub args: Vec<Val>,
    pub props: Vec<(String, Val)>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn prop(&self, k: &str) -> Option<&Val> {
        self.props.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }
    pub fn prop_str(&self, k: &str) -> Option<&str> {
        match self.prop(k) {
            Some(Val::Str(s)) => Some(s),
            _ => None,
        }
    }
    pub fn prop_int(&self, k: &str) -> Option<i64> {
        match self.prop(k) {
            Some(Val::Int(i)) => Some(*i),
            _ => None,
        }
    }
    pub fn prop_bool(&self, k: &str) -> Option<bool> {
        match self.prop(k) {
            Some(Val::Bool(b)) => Some(*b),
            _ => None,
        }
    }
    pub fn str_arg(&self, i: usize) -> Result<&str, String> {
        match self.args.get(i) {
            Some(Val::Str(s)) => Ok(s),
            _ => Err(format!("{}: argument {i} must be a string", self.name)),
        }
    }
    pub fn int_arg(&self, i: usize) -> Result<i64, String> {
        match self.args.get(i) {
            Some(Val::Int(v)) => Ok(*v),
            _ => Err(format!("{}: argument {i} must be an integer", self.name)),
        }
    }
    pub fn strs(&self) -> Vec<String> {
        self.args
            .iter()
            .filter_map(|a| match a {
                Val::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }
}

struct Cur<'a> {
    s: &'a [char],
    i: usize,
}

impl Cur<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn skip_inline(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' {
                self.i += 1;
            } else if c == '/' && self.s.get(self.i + 1) == Some(&'/') {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }
    fn skip_all(&mut self) {
        loop {
            self.skip_inline();
            match self.peek() {
                Some('\n' | '\r' | ';') => self.i += 1,
                _ => break,
            }
        }
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '?' | '+')
}

fn string(c: &mut Cur) -> Result<String, String> {
    c.i += 1;
    let mut out = String::new();
    loop {
        let ch = c.peek().ok_or("unterminated string")?;
        c.i += 1;
        match ch {
            '"' => return Ok(out),
            '\\' => {
                let e = c.peek().ok_or("bad escape")?;
                c.i += 1;
                match e {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'e' => out.push('\x1b'),
                    'a' => out.push('\x07'),
                    'b' => out.push('\x08'),
                    '0' => out.push('\0'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    'x' => {
                        let h: String = c.s[c.i..(c.i + 2).min(c.s.len())].iter().collect();
                        c.i += 2;
                        let v = u32::from_str_radix(&h, 16).map_err(|_| "bad \\x")?;
                        if v > 0x7f {
                            return Err("\\x above 7f: use feed-hex".into());
                        }
                        out.push(char::from_u32(v).ok_or("bad \\x")?);
                    }
                    'u' => {
                        if c.peek() != Some('{') {
                            return Err("bad \\u".into());
                        }
                        c.i += 1;
                        let mut h = String::new();
                        while let Some(d) = c.peek() {
                            c.i += 1;
                            if d == '}' {
                                break;
                            }
                            h.push(d);
                        }
                        let v = u32::from_str_radix(&h, 16).map_err(|_| "bad \\u")?;
                        out.push(char::from_u32(v).ok_or("bad \\u scalar")?);
                    }
                    other => return Err(format!("unknown escape \\{other}")),
                }
            }
            _ => out.push(ch),
        }
    }
}

fn value(c: &mut Cur) -> Result<Val, String> {
    match c.peek() {
        Some('"') => Ok(Val::Str(string(c)?)),
        Some('#') => {
            let start = c.i;
            c.i += 1;
            while c.peek().is_some_and(is_ident) {
                c.i += 1;
            }
            let w: String = c.s[start..c.i].iter().collect();
            match w.as_str() {
                "#true" => Ok(Val::Bool(true)),
                "#false" => Ok(Val::Bool(false)),
                _ => Err(format!("bad keyword {w}")),
            }
        }
        Some(ch) if ch.is_ascii_digit() || ch == '-' => {
            let start = c.i;
            c.i += 1;
            while c.peek().is_some_and(|d| d.is_ascii_digit()) {
                c.i += 1;
            }
            let w: String = c.s[start..c.i].iter().collect();
            w.parse().map(Val::Int).map_err(|_| format!("bad int {w}"))
        }
        other => Err(format!("bad value start {other:?}")),
    }
}

fn nodes(c: &mut Cur, top: bool) -> Result<Vec<Node>, String> {
    let mut out = Vec::new();
    loop {
        c.skip_all();
        match c.peek() {
            None => {
                if top {
                    return Ok(out);
                }
                return Err("missing }".into());
            }
            Some('}') => {
                if top {
                    return Err("stray }".into());
                }
                c.i += 1;
                return Ok(out);
            }
            _ => {}
        }
        let mut n = Node::default();
        if c.peek() == Some('"') {
            n.name = string(c)?;
        } else {
            let start = c.i;
            while c.peek().is_some_and(is_ident) {
                c.i += 1;
            }
            if start == c.i {
                return Err(format!("bad node start {:?} at {}", c.peek(), c.i));
            }
            n.name = c.s[start..c.i].iter().collect();
        }
        loop {
            c.skip_inline();
            match c.peek() {
                None | Some('\n' | '\r' | ';' | '}') => break,
                Some('{') => {
                    c.i += 1;
                    n.children = nodes(c, false)?;
                    break;
                }
                Some('"' | '#') => n.args.push(value(c)?),
                Some(ch) if ch.is_ascii_digit() || ch == '-' => n.args.push(value(c)?),
                Some(ch) if is_ident(ch) => {
                    let start = c.i;
                    while c.peek().is_some_and(is_ident) {
                        c.i += 1;
                    }
                    let key: String = c.s[start..c.i].iter().collect();
                    if c.peek() == Some('=') {
                        c.i += 1;
                        let v = value(c)?;
                        n.props.push((key, v));
                    } else {
                        n.args.push(Val::Str(key));
                    }
                }
                Some(ch) => return Err(format!("unexpected {ch:?} in node {}", n.name)),
            }
        }
        out.push(n);
    }
}

pub fn parse(src: &str) -> Result<Vec<Node>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut c = Cur { s: &chars, i: 0 };
    nodes(&mut c, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nodes() {
        let n = parse("a \"x\\e\" 3 k=#true { b; c 1 }\n// c\nd").unwrap();
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].args[0], Val::Str("x\x1b".into()));
        assert_eq!(n[0].prop_bool("k"), Some(true));
        assert_eq!(n[0].children.len(), 2);
        assert_eq!(n[1].name, "d");
    }
}
