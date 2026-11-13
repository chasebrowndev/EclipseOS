// SPDX-License-Identifier: AGPL-3.0-only
//! Small validators for the string-shaped types: colours, ranges, adjusts, argv placeholders,
//! templates, regexes, match expressions and action ids.
//!
//! The full match and template grammars belong to `apoth-action` (T-11 §7, §8); these check
//! the structure only, so a bad value is caught at load. Spec gap recorded in CLAUDE.md.

/// Parse `#rrggbb` or `#rrggbbaa`.
pub fn parse_color(s: &str) -> Option<[u8; 4]> {
    let h = s.strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if h.len() == 8 { byte(6)? } else { 255 },
    ])
}

/// `#rrggbb`, or `#rrggbbaa` when not opaque.
pub fn format_color(c: [u8; 4]) -> String {
    if c[3] == 255 {
        format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
    }
}

/// `U+E0A0`, `U+E0A0-E0A3`.
pub fn validate_codepoint_range(s: &str) -> Result<(u32, u32), String> {
    let body = s
        .strip_prefix("U+")
        .or_else(|| s.strip_prefix("u+"))
        .ok_or_else(|| format!("range `{s}` must start with U+"))?;
    let hex = |h: &str| -> Result<u32, String> {
        if h.len() < 4 || h.len() > 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("bad codepoint `{h}` in `{s}`"));
        }
        let v = u32::from_str_radix(h, 16).map_err(|_| format!("bad codepoint `{h}`"))?;
        if v > 0x10FFFF {
            return Err(format!("codepoint `{h}` is beyond U+10FFFF"));
        }
        Ok(v)
    };
    match body.split_once('-') {
        Some((a, b)) => {
            let (a, b) = (hex(a)?, hex(b)?);
            if a > b {
                return Err(format!("range `{s}` is reversed"));
            }
            Ok((a, b))
        }
        None => {
            let a = hex(body)?;
            Ok((a, a))
        }
    }
}

/// `3px`, `-10%`, `+0.5px`: a signed length with an explicit unit.
pub fn validate_adjust(s: &str) -> Result<(), String> {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let num = body
        .strip_suffix("px")
        .or_else(|| body.strip_suffix('%'))
        .ok_or_else(|| "needs an explicit unit: px or %".to_string())?;
    let ok = !num.is_empty()
        && num.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && num.matches('.').count() <= 1
        && !num.starts_with('.')
        && !num.ends_with('.');
    if ok {
        Ok(())
    } else {
        Err("not a number followed by px or %".into())
    }
}

/// The T-11 §2 typed placeholders.
pub const PLACEHOLDERS: &[&str] = &[
    "selection",
    "url",
    "match",
    "cwd",
    "file",
    "line",
    "pane-id",
];

/// Check the placeholders inside one argv element. Delegates to `apoth-action` so what
/// loads clean also runs: a placeholder must be a whole element.
pub fn validate_argv_element(s: &str) -> Result<(), String> {
    apoth_action::Argv::from_source(&["x".to_string(), s.to_string()]).map(|_| ())
}

/// Validate an argv (element count, size, NUL, placeholders) with `apoth-action`'s parser.
pub fn validate_argv(args: &[String]) -> Result<(), String> {
    // An empty argv means "no command" for the optional argv keys.
    if args.is_empty() {
        return Ok(());
    }
    if args.first().is_some_and(|f| f.is_empty()) {
        return Err("argv[0] is empty".into());
    }
    apoth_action::Argv::from_source(args).map(|_| ())
}

/// Template structure: balanced `{…}` with `{if …}`, `{else}`, `{end}` properly nested.
pub fn validate_template(s: &str) -> Result<(), String> {
    let mut depth = 0i32;
    let mut seen_else: Vec<bool> = Vec::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '{' if b.get(i + 1) == Some(&'{') => i += 2,
            '}' if b.get(i + 1) == Some(&'}') => i += 2,
            '{' => {
                let mut j = i + 1;
                let mut quoted = false;
                while j < b.len() && (quoted || b[j] != '}') {
                    if b[j] == '\\' && quoted {
                        // An escaped character inside a string never closes it.
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
                if inner == "if" || inner.starts_with("if ") {
                    depth += 1;
                    seen_else.push(false);
                } else if inner == "else" {
                    match seen_else.last_mut() {
                        Some(e) if !*e => *e = true,
                        _ => return Err("`{else}` without a matching `{if}`".into()),
                    }
                } else if inner == "end" {
                    if depth == 0 {
                        return Err("`{end}` without a matching `{if}`".into());
                    }
                    depth -= 1;
                    seen_else.pop();
                }
                i = j + 1;
            }
            '}' => return Err("stray `}` in template".into()),
            _ => i += 1,
        }
    }
    if depth != 0 {
        return Err("`{if}` without `{end}`".into());
    }
    Ok(())
}

/// Compile the pattern with the regex engine the runtime uses (`regex-automata`'s meta
/// engine, linear time, no backreferences or look-around). A pattern that loads clean
/// therefore compiles; there is no structural stand-in to drift from it.
pub fn validate_regex(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("empty pattern".into());
    }
    if s.len() > MAX_REGEX_BYTES {
        return Err(format!("pattern is longer than {MAX_REGEX_BYTES} bytes"));
    }
    regex_automata::meta::Builder::new()
        .syntax(regex_automata::util::syntax::Config::new().nest_limit(32))
        .build(s)
        .map(|_| ())
        .map_err(|e| {
            // The builder's message may quote the pattern; keep only its first line.
            let m = e.to_string();
            m.lines().next().unwrap_or("invalid pattern").to_string()
        })
}

/// Longest regex accepted in a config value.
pub const MAX_REGEX_BYTES: usize = 4096;

/// Fields of the match language (T-11 §7); `var:` and `env:` take a name.
pub const MATCH_FIELDS: &[&str] = &[
    "id",
    "title",
    "pid",
    "cwd",
    "cmdline",
    "process",
    "host",
    "user",
    "profile",
    "tab",
    "window",
    "workspace",
    "state",
    "status",
    "duration",
    "command",
];

#[derive(Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Word(String),
}

fn match_tokens(s: &str) -> Result<Vec<Tok>, String> {
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

/// Bare state words usable as whole terms (`not focused`).
pub const MATCH_BARE: &[&str] = &["focused", "active", "zoomed", "bell", "running"];

fn match_term(word: &str) -> Result<(), String> {
    if MATCH_BARE.contains(&word) {
        return Ok(());
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
        return Ok(());
    }
    if !MATCH_FIELDS.contains(&field) {
        return Err(match crate::diag::suggest(field, MATCH_FIELDS) {
            Some(s) => format!("unknown match field `{field}` (did you mean `{s}`?)"),
            None => format!("unknown match field `{field}`"),
        });
    }
    if rest.is_empty() {
        return Err(format!("empty pattern for `{field}`"));
    }
    Ok(())
}

fn match_expr(t: &[Tok], i: &mut usize) -> Result<(), String> {
    match_unary(t, i)?;
    while let Some(Tok::Word(w)) = t.get(*i) {
        if w == "and" || w == "or" {
            *i += 1;
            match_unary(t, i)?;
        } else {
            break;
        }
    }
    Ok(())
}

fn match_unary(t: &[Tok], i: &mut usize) -> Result<(), String> {
    match t.get(*i) {
        Some(Tok::Word(w)) if w == "not" => {
            *i += 1;
            match_unary(t, i)
        }
        Some(Tok::LParen) => {
            *i += 1;
            match_expr(t, i)?;
            if t.get(*i) == Some(&Tok::RParen) {
                *i += 1;
                Ok(())
            } else {
                Err("missing `)`".into())
            }
        }
        Some(Tok::Word(w)) => {
            match_term(w)?;
            *i += 1;
            Ok(())
        }
        Some(Tok::RParen) => Err("unexpected `)`".into()),
        None => Err("expression ends early".into()),
    }
}

/// Structure of a match expression: terms `field:pattern` joined by `and`, `or`, `not`, parens.
pub fn validate_match(s: &str) -> Result<(), String> {
    let toks = match_tokens(s)?;
    if toks.is_empty() {
        return Err("empty match expression".into());
    }
    let mut i = 0;
    match_expr(&toks, &mut i)?;
    if i != toks.len() {
        return Err("unexpected trailing input in match expression".into());
    }
    Ok(())
}

/// Closed action families (T-11 §1). `launch`, `open`, `pipe`, `capture`, `notify` stand alone.
pub const ACTION_FAMILIES: &[&str] = &[
    "input",
    "clipboard",
    "selection",
    "scroll",
    "search",
    "copy-mode",
    "key-table",
    "cursor",
    "hints",
    "prompt",
    "block",
    "mark",
    "bookmark",
    "palette",
    "interface",
    "pane",
    "tab",
    "window",
    "workspace",
    "layout",
    "broadcast",
    "keyboard",
    "font",
    "theme",
    "config",
    "var",
    "session",
    "scrollback",
    "coprocess",
    "macro",
    "media",
    "graphics",
    "debug",
    "app",
];

/// Action ids that have no family prefix in the T-11 §1 table.
pub const STANDALONE_ACTIONS: &[&str] = &["launch", "open", "pipe", "capture", "notify"];

/// Is `id` a well-formed id in a closed family? (Whether the verb exists is the registry's
/// business, in `apoth-action`.)
pub fn validate_action_id(id: &str) -> Result<(), String> {
    if STANDALONE_ACTIONS.contains(&id) {
        return Ok(());
    }
    let (family, verb) = id
        .split_once('.')
        .ok_or_else(|| format!("`{id}` is not `family.verb`"))?;
    if !ACTION_FAMILIES.contains(&family) {
        return Err(match crate::diag::suggest(family, ACTION_FAMILIES) {
            Some(s) => format!("unknown action family `{family}` (did you mean `{s}`?)"),
            None => format!("unknown action family `{family}`"),
        });
    }
    let kebab = !verb.is_empty()
        && verb.split('-').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    if !kebab {
        return Err(format!("verb `{verb}` must be kebab-case"));
    }
    Ok(())
}

/// Shell program names refused as `socket.grants` paths (T-12 §4).
pub const INTERPRETERS: &[&str] = &[
    "node", "deno", "bun", "perl", "ruby", "php", "sh", "bash", "zsh", "fish", "nu", "dash", "env",
    "busybox",
];

/// Is this basename an interpreter (`python*`, `lua*`, or in [`INTERPRETERS`] or `extra`)?
pub fn is_interpreter(path: &str, extra: &[String]) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.starts_with("python")
        || base.starts_with("lua")
        || INTERPRETERS.contains(&base)
        || extra.iter().any(|e| e == base)
}

/// Capability classes (T-12 §4).
pub const CAPABILITY_CLASSES: &[&str] = &[
    "observe",
    "layout",
    "appearance",
    "spawn",
    "read-content",
    "input",
    "clipboard",
    "keys",
    "config-write",
];

/// The T-18 §3 sequence policy classes.
pub const SEQUENCE_CLASSES: &[&str] = &[
    "clipboard-write",
    "clipboard-read",
    "primary-selection",
    "title-set",
    "title-report",
    "window-ops-report",
    "window-ops-control",
    "color",
    "font",
    "notifications",
    "notification-activation",
    "pointer-shape",
    "graphics-direct",
    "graphics-file",
    "media-path",
    "user-vars",
    "hyperlinks",
    "input-modes",
];

/// `allow`, `ask`, `deny`.
pub const DECISIONS: &[&str] = &["allow", "ask", "deny"];
