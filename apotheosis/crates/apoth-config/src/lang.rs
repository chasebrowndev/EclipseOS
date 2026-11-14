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

/// A template parses (T-11 §8): balanced `{…}`, known filters, `{if}`/`{else}`/`{end}`
/// properly nested, conditions in the template condition grammar.
pub fn validate_template(s: &str) -> Result<(), String> {
    crate::template::Template::parse(s).map(|_| ())
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

/// Bare state words usable as whole terms (`not focused`).
pub const MATCH_BARE: &[&str] = &["focused", "active", "zoomed", "bell", "running"];

/// Nesting bound of a match expression (parentheses plus `not`): the parser recurses per
/// level, so an unbounded depth from a hostile config would overflow the stack (T-18 §5).
pub const MATCH_MAX_DEPTH: usize = 64;

/// Structure of a match expression: terms `field:pattern` joined by `and`, `or`, `not`, parens.
pub fn validate_match(s: &str) -> Result<(), String> {
    crate::expr::Expr::parse(s).map(|_| ())
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
