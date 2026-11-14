// SPDX-License-Identifier: AGPL-3.0-only
//! MIME detection for "Open with" (T-14 §11): name globs plus a magic sniff of the first bytes,
//! from the shared-mime-info database's generated files (`globs2`, `magic`, `subclasses`,
//! `aliases`, read by the published shared-mime-info specification; no C library, no XML), with
//! a small built-in table when no database is installed.
//!
//! Order, per the specification's recommendation: a unique glob result wins; several glob
//! results are settled by the sniff; no glob result asks the sniff alone; nothing at all is
//! `text/plain` for bytes that look like text, else `application/octet-stream`.
//!
//! File names and bytes are user content: nothing here logs them.

use std::collections::HashMap;

/// Bytes read from a file for the sniff (the database's rules look at most this far that we
/// honour; a rule past it does not match).
pub(crate) const SNIFF_BYTES: usize = 8192;
/// Largest database file read.
pub(crate) const MAX_DB_FILE: usize = 4 << 20;
/// Most glob or magic entries kept (a hostile database cannot grow us without bound).
const MAX_ENTRIES: usize = 65_536;
/// Deepest magic rule nesting honoured.
const MAX_INDENT: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Glob {
    weight: u32,
    mime: String,
    /// Lowercased unless `cs`.
    pattern: String,
    cs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rule {
    indent: usize,
    offset: usize,
    range: usize,
    value: Vec<u8>,
    mask: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Magic {
    priority: u32,
    mime: String,
    rules: Vec<Rule>,
}

/// The loaded database (or the built-in table).
#[derive(Clone, Debug, Default)]
pub(crate) struct MimeDb {
    globs: Vec<Glob>,
    magic: Vec<Magic>,
    parents: HashMap<String, Vec<String>>,
    aliases: HashMap<String, String>,
}

/// Built-in globs: `(weight, mime, pattern)`, used when no database is installed.
const BUILTIN_GLOBS: &[(u32, &str, &str)] = &[
    (50, "image/png", "*.png"),
    (50, "image/jpeg", "*.jpg"),
    (50, "image/jpeg", "*.jpeg"),
    (50, "image/gif", "*.gif"),
    (50, "image/webp", "*.webp"),
    (50, "image/bmp", "*.bmp"),
    (50, "image/svg+xml", "*.svg"),
    (50, "image/avif", "*.avif"),
    (50, "image/tiff", "*.tif"),
    (50, "image/tiff", "*.tiff"),
    (50, "image/vnd.microsoft.icon", "*.ico"),
    (50, "video/mp4", "*.mp4"),
    (50, "video/mp4", "*.m4v"),
    (50, "video/x-matroska", "*.mkv"),
    (50, "video/webm", "*.webm"),
    (50, "video/quicktime", "*.mov"),
    (50, "video/x-msvideo", "*.avi"),
    (50, "audio/mpeg", "*.mp3"),
    (50, "audio/flac", "*.flac"),
    (50, "audio/ogg", "*.ogg"),
    (50, "audio/x-opus+ogg", "*.opus"),
    (50, "audio/x-wav", "*.wav"),
    (50, "audio/mp4", "*.m4a"),
    (50, "application/pdf", "*.pdf"),
    (50, "text/html", "*.html"),
    (50, "text/html", "*.htm"),
    (50, "text/markdown", "*.md"),
    (50, "text/plain", "*.txt"),
    (50, "application/json", "*.json"),
    (50, "application/zip", "*.zip"),
    (50, "application/x-tar", "*.tar"),
    (50, "application/gzip", "*.gz"),
];

/// Built-in magic: `(priority, mime, offset, bytes)`, one rule each.
const BUILTIN_MAGIC: &[(u32, &str, usize, &[u8])] = &[
    (50, "image/png", 0, b"\x89PNG\r\n\x1a\n"),
    (50, "image/jpeg", 0, b"\xff\xd8\xff"),
    (50, "image/gif", 0, b"GIF8"),
    (50, "application/pdf", 0, b"%PDF-"),
    (50, "application/zip", 0, b"PK\x03\x04"),
    (50, "video/x-matroska", 0, b"\x1a\x45\xdf\xa3"),
    (50, "audio/ogg", 0, b"OggS"),
    (50, "audio/flac", 0, b"fLaC"),
    (50, "audio/mpeg", 0, b"ID3"),
    (50, "application/x-executable", 0, b"\x7fELF"),
    (50, "application/gzip", 0, b"\x1f\x8b"),
];

/// Built-in subclass pairs (child, parent).
const BUILTIN_PARENTS: &[(&str, &str)] = &[
    ("image/svg+xml", "application/xml"),
    ("text/html", "text/plain"),
    ("text/markdown", "text/plain"),
    ("application/json", "text/plain"),
    ("application/xml", "text/plain"),
];

impl MimeDb {
    /// The built-in table (no database installed).
    pub(crate) fn builtin() -> MimeDb {
        let mut db = MimeDb::default();
        for (w, m, p) in BUILTIN_GLOBS {
            db.globs.push(Glob {
                weight: *w,
                mime: (*m).to_string(),
                pattern: (*p).to_string(),
                cs: false,
            });
        }
        // RIFF and ISO BMFF containers need two rules: the box/chunk tag and the brand.
        let two = |m: &str, a: (usize, &[u8]), b: (usize, &[u8])| Magic {
            priority: 50,
            mime: m.to_string(),
            rules: vec![
                Rule {
                    indent: 0,
                    offset: a.0,
                    range: 1,
                    value: a.1.to_vec(),
                    mask: None,
                },
                Rule {
                    indent: 1,
                    offset: b.0,
                    range: 1,
                    value: b.1.to_vec(),
                    mask: None,
                },
            ],
        };
        db.magic.push(two("image/webp", (0, b"RIFF"), (8, b"WEBP")));
        db.magic
            .push(two("audio/x-wav", (0, b"RIFF"), (8, b"WAVE")));
        db.magic
            .push(two("video/x-msvideo", (0, b"RIFF"), (8, b"AVI ")));
        db.magic.push(two("image/avif", (4, b"ftyp"), (8, b"avif")));
        db.magic.push(two("audio/mp4", (4, b"ftyp"), (8, b"M4A ")));
        db.magic.push(Magic {
            priority: 40,
            mime: "video/mp4".into(),
            rules: vec![Rule {
                indent: 0,
                offset: 4,
                range: 1,
                value: b"ftyp".to_vec(),
                mask: None,
            }],
        });
        for (p, m, o, v) in BUILTIN_MAGIC {
            db.magic.push(Magic {
                priority: *p,
                mime: (*m).to_string(),
                rules: vec![Rule {
                    indent: 0,
                    offset: *o,
                    range: 1,
                    value: v.to_vec(),
                    mask: None,
                }],
            });
        }
        db.magic.sort_by_key(|m| std::cmp::Reverse(m.priority));
        for (c, p) in BUILTIN_PARENTS {
            db.parents
                .entry((*c).to_string())
                .or_default()
                .push((*p).to_string());
        }
        db
    }

    /// Add one directory's `globs2` text (call highest-precedence directory first).
    pub(crate) fn add_globs2(&mut self, text: &str) {
        for line in text.lines() {
            if self.globs.len() >= MAX_ENTRIES {
                return;
            }
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.splitn(4, ':');
            let (Some(w), Some(m), Some(p)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let Ok(weight) = w.parse::<u32>() else {
                continue;
            };
            // `__NOGLOBS__` (a higher directory dropping a type's globs) is not honoured: it
            // only ever removes candidates, and the sniff still decides.
            if p == "__NOGLOBS__" || m.is_empty() || p.is_empty() {
                continue;
            }
            let cs = it.next().is_some_and(|f| f.split(',').any(|f| f == "cs"));
            let pattern = if cs { p.to_string() } else { p.to_lowercase() };
            let g = Glob {
                weight,
                mime: m.to_string(),
                pattern,
                cs,
            };
            if !self.globs.contains(&g) {
                self.globs.push(g);
            }
        }
    }

    /// Add one directory's `subclasses` text (`child parent` per line).
    pub(crate) fn add_subclasses(&mut self, text: &str) {
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if let (Some(c), Some(p)) = (it.next(), it.next()) {
                let v = self.parents.entry(c.to_string()).or_default();
                if v.len() < 16 && !v.iter().any(|x| x == p) {
                    v.push(p.to_string());
                }
            }
        }
    }

    /// Add one directory's `aliases` text (`alias canonical` per line).
    pub(crate) fn add_aliases(&mut self, text: &str) {
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if let (Some(a), Some(c)) = (it.next(), it.next()) {
                self.aliases
                    .entry(a.to_string())
                    .or_insert_with(|| c.to_string());
            }
        }
    }

    /// Add one directory's binary `magic` file. A malformed section is skipped from the
    /// error to the next section header; a bad header ignores the file.
    pub(crate) fn add_magic(&mut self, data: &[u8]) {
        const HEADER: &[u8] = b"MIME-Magic\0\n";
        let Some(mut p) = data.strip_prefix(HEADER) else {
            return;
        };
        let mut cur: Option<Magic> = None;
        while !p.is_empty() {
            if self.magic.len() >= MAX_ENTRIES {
                break;
            }
            if p[0] == b'[' {
                if let Some(m) = cur.take()
                    && !m.rules.is_empty()
                {
                    self.magic.push(m);
                }
                let Some(end) = p.iter().position(|&b| b == b'\n') else {
                    break;
                };
                let head = &p[1..end];
                p = &p[end + 1..];
                let Some(head) = head.strip_suffix(b"]") else {
                    continue;
                };
                let head = String::from_utf8_lossy(head);
                if let Some((pr, m)) = head.split_once(':')
                    && let Ok(priority) = pr.parse::<u32>()
                {
                    cur = Some(Magic {
                        priority,
                        mime: m.to_string(),
                        rules: Vec::new(),
                    });
                }
                continue;
            }
            match parse_rule(p) {
                Some((rule, rest)) => {
                    if let Some(m) = cur.as_mut()
                        && m.rules.len() < 256
                    {
                        m.rules.push(rule);
                    }
                    p = rest;
                }
                None => {
                    // Skip to the next section.
                    cur = None;
                    match p.windows(2).position(|w| w == b"\n[") {
                        Some(i) => p = &p[i + 1..],
                        None => break,
                    }
                }
            }
        }
        if let Some(m) = cur
            && !m.rules.is_empty()
        {
            self.magic.push(m);
        }
        // Stable: equal priorities keep file (directory precedence) order.
        self.magic.sort_by_key(|m| std::cmp::Reverse(m.priority));
    }

    fn canonical<'a>(&'a self, m: &'a str) -> &'a str {
        self.aliases.get(m).map_or(m, String::as_str)
    }

    /// Glob candidates for a file name: the highest weight, then the longest pattern.
    fn glob_candidates(&self, name: &str) -> Vec<&str> {
        let lower = name.to_lowercase();
        let mut best: Option<(u32, usize)> = None;
        let mut out: Vec<&str> = Vec::new();
        for g in &self.globs {
            let n = if g.cs { name } else { lower.as_str() };
            if !glob_match(g.pattern.as_bytes(), n.as_bytes()) {
                continue;
            }
            let k = (g.weight, g.pattern.len());
            match best {
                Some(b) if k < b => {}
                Some(b) if k == b => {
                    let m = self.canonical(&g.mime);
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
                _ => {
                    best = Some(k);
                    out.clear();
                    out.push(self.canonical(&g.mime));
                }
            }
        }
        out
    }

    /// The magic result for `head`, highest priority first.
    pub(crate) fn sniff(&self, head: &[u8]) -> Option<&str> {
        self.magic
            .iter()
            .find(|m| section_matches(&m.rules, head))
            .map(|m| self.canonical(&m.mime))
    }

    /// The type of a regular file named `name` whose first bytes are `head`.
    pub(crate) fn detect(&self, name: &str, head: &[u8]) -> String {
        let globs = self.glob_candidates(name);
        if globs.len() == 1 {
            return globs[0].to_string();
        }
        if let Some(m) = self.sniff(head) {
            if globs.is_empty() {
                return m.to_string();
            }
            if let Some(g) = globs.iter().find(|g| **g == m || self.is_a(m, g)) {
                return (*g).to_string();
            }
        }
        if let Some(g) = globs.first() {
            return (*g).to_string();
        }
        if looks_text(head) {
            "text/plain".to_string()
        } else {
            "application/octet-stream".to_string()
        }
    }

    /// Ancestors of `m`, nearest first, breadth-first, bounded: declared subclasses, then the
    /// specification's implicit ones (`text/*` is a `text/plain`; every non-`inode/` type is an
    /// `application/octet-stream`).
    pub(crate) fn ancestors(&self, m: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut queue: std::collections::VecDeque<String> = [m.to_string()].into();
        while let Some(t) = queue.pop_front() {
            if out.len() >= 32 {
                break;
            }
            let t = self.canonical(&t).to_string();
            for p in self.parents.get(&t).into_iter().flatten() {
                if p != m && !out.contains(p) {
                    out.push(p.clone());
                    queue.push_back(p.clone());
                }
            }
        }
        if m.starts_with("text/") && m != "text/plain" && !out.iter().any(|x| x == "text/plain") {
            out.push("text/plain".into());
        }
        if !m.starts_with("inode/")
            && m != "application/octet-stream"
            && !out.iter().any(|x| x == "application/octet-stream")
        {
            out.push("application/octet-stream".into());
        }
        out
    }

    /// `m` is `parent` or one of its subclasses.
    pub(crate) fn is_a(&self, m: &str, parent: &str) -> bool {
        m == parent || self.ancestors(m).iter().any(|a| a == parent)
    }
}

/// One rule line: `[indent]>offset=LLvalue[&mask][~word][+range]\n`.
fn parse_rule(p: &[u8]) -> Option<(Rule, &[u8])> {
    let mut i = 0;
    let digits = |p: &[u8], i: &mut usize| -> Option<usize> {
        let s = *i;
        while *i < p.len() && p[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == s || *i - s > 9 {
            return None;
        }
        std::str::from_utf8(&p[s..*i]).ok()?.parse().ok()
    };
    let indent = if p.first()?.is_ascii_digit() {
        digits(p, &mut i)?
    } else {
        0
    };
    if indent > MAX_INDENT || *p.get(i)? != b'>' {
        return None;
    }
    i += 1;
    let offset = digits(p, &mut i)?;
    if *p.get(i)? != b'=' {
        return None;
    }
    i += 1;
    let len = usize::from(u16::from_be_bytes([*p.get(i)?, *p.get(i + 1)?]));
    i += 2;
    if len == 0 {
        return None;
    }
    let value = p.get(i..i + len)?.to_vec();
    i += len;
    let mut mask = None;
    let mut word = 1;
    let mut range = 1;
    loop {
        match *p.get(i)? {
            b'&' => {
                mask = Some(p.get(i + 1..i + 1 + len)?.to_vec());
                i += 1 + len;
            }
            b'~' => {
                i += 1;
                word = digits(p, &mut i)?;
            }
            b'+' => {
                i += 1;
                range = digits(p, &mut i)?.clamp(1, SNIFF_BYTES);
            }
            b'\n' => {
                i += 1;
                break;
            }
            _ => return None,
        }
    }
    let mut value = value;
    // Word-sized values are stored big-endian; we compare in host (little-endian) order.
    if cfg!(target_endian = "little") && (word == 2 || word == 4) && len % word == 0 {
        for c in value.chunks_mut(word) {
            c.reverse();
        }
        if let Some(m) = mask.as_mut() {
            for c in m.chunks_mut(word) {
                c.reverse();
            }
        }
    }
    Some((
        Rule {
            indent,
            offset,
            range,
            value,
            mask,
        },
        &p[i..],
    ))
}

fn rule_matches(r: &Rule, head: &[u8]) -> bool {
    let n = r.value.len();
    (r.offset..r.offset.saturating_add(r.range)).any(|o| {
        let Some(w) = head.get(o..o + n) else {
            return false;
        };
        match &r.mask {
            None => w == r.value.as_slice(),
            Some(m) => w
                .iter()
                .zip(&r.value)
                .zip(m)
                .all(|((b, v), k)| b & k == v & k),
        }
    })
}

/// A section matches when one of its top-level rules matches with (when it has nested rules)
/// one of its children matching, recursively.
fn section_matches(rules: &[Rule], head: &[u8]) -> bool {
    fn at(rules: &[Rule], i: usize, head: &[u8]) -> bool {
        if !rule_matches(&rules[i], head) {
            return false;
        }
        let depth = rules[i].indent;
        let mut j = i + 1;
        let mut has_child = false;
        while j < rules.len() && rules[j].indent > depth {
            if rules[j].indent == depth + 1 {
                has_child = true;
                if at(rules, j, head) {
                    return true;
                }
            }
            j += 1;
        }
        !has_child
    }
    (0..rules.len()).any(|i| rules[i].indent == 0 && at(rules, i, head))
}

/// `fnmatch`-style glob: `*`, `?`, `[...]` (with `!`/`^` negation and ranges).
pub(crate) fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < s.len() {
        if p < pat.len() {
            match pat[p] {
                b'*' => {
                    star = Some((p, i));
                    p += 1;
                    continue;
                }
                b'?' => {
                    p += 1;
                    i += 1;
                    continue;
                }
                b'[' => match class(pat, p, s[i]) {
                    Some((true, next)) => {
                        p = next;
                        i += 1;
                        continue;
                    }
                    Some((false, _)) => {}
                    // Unterminated: a literal `[`.
                    None if s[i] == b'[' => {
                        p += 1;
                        i += 1;
                        continue;
                    }
                    None => {}
                },
                c if c == s[i] => {
                    p += 1;
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, si)) => {
                p = sp + 1;
                i = si + 1;
                star = Some((sp, si + 1));
            }
            None => return false,
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// Match one byte against the bracket class starting at `pat[p] == '['`: `(matched, index
/// after ']')`, or `None` for an unterminated class (then `[` is literal).
fn class(pat: &[u8], p: usize, c: u8) -> Option<(bool, usize)> {
    let mut j = p + 1;
    let neg = matches!(pat.get(j), Some(b'!' | b'^'));
    if neg {
        j += 1;
    }
    let mut hit = false;
    let mut first = true;
    while j < pat.len() && (first || pat[j] != b']') {
        first = false;
        if j + 2 < pat.len() && pat[j + 1] == b'-' && pat[j + 2] != b']' {
            if pat[j] <= c && c <= pat[j + 2] {
                hit = true;
            }
            j += 3;
        } else {
            if pat[j] == c {
                hit = true;
            }
            j += 1;
        }
    }
    if j >= pat.len() {
        return None;
    }
    Some((hit != neg, j + 1))
}

/// Text by the specification's heuristic: no NUL and valid UTF-8 (a cut multi-byte sequence
/// at the end of the sample is allowed). Empty files are text.
pub(crate) fn looks_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

/// `pattern` (`type/subtype` or `type/*`) covers `m` directly (no subclassing).
pub(crate) fn pattern_covers(pattern: &str, m: &str) -> bool {
    match pattern.strip_suffix("/*") {
        Some(t) => m
            .split_once('/')
            .is_some_and(|(mt, _)| mt.eq_ignore_ascii_case(t)),
        None => pattern.eq_ignore_ascii_case(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn magic_file(sections: &[(&str, &[u8])]) -> Vec<u8> {
        let mut v = b"MIME-Magic\0\n".to_vec();
        for (h, body) in sections {
            v.extend_from_slice(format!("[{h}]\n").as_bytes());
            v.extend_from_slice(body);
        }
        v
    }

    fn rule(indent: &str, off: usize, val: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut v = format!("{indent}>{off}=").into_bytes();
        v.extend_from_slice(&(val.len() as u16).to_be_bytes());
        v.extend_from_slice(val);
        v.extend_from_slice(tail);
        v.push(b'\n');
        v
    }

    #[test]
    fn globs() {
        assert!(glob_match(b"*.png", b"a.png"));
        assert!(!glob_match(b"*.png", b"a.pngx"));
        assert!(glob_match(b"makefile", b"makefile"));
        assert!(glob_match(b"*.[ch]", b"x.h"));
        assert!(!glob_match(b"*.[!ch]", b"x.h"));
        assert!(glob_match(b"*.[a-c]x", b"y.bx"));
        assert!(glob_match(b"README*", b"README.md"));
        assert!(glob_match(b"?.c", b"a.c"));
        assert!(glob_match(b"*.tar.*", b"a.tar.gz"));
        assert!(!glob_match(b"[abc", b"a"));
        assert!(glob_match(b"[abc", b"[abc"));
    }

    #[test]
    fn builtin_detects_by_name_and_content() {
        let db = MimeDb::builtin();
        assert_eq!(db.detect("cat.PNG", b""), "image/png");
        assert_eq!(db.detect("noext", b"\x89PNG\r\n\x1a\n...."), "image/png");
        assert_eq!(db.detect("noext", b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        assert_eq!(db.detect("noext", b"\0\0\0\x20ftypisom"), "video/mp4");
        assert_eq!(db.detect("noext", b"\0\0\0\x20ftypavif"), "image/avif");
        assert_eq!(
            db.detect("clip", b"\x1a\x45\xdf\xa3\x01"),
            "video/x-matroska"
        );
        assert_eq!(db.detect("notes", b"hello\nworld\n"), "text/plain");
        assert_eq!(
            db.detect("blob", b"\x00\x01\x02"),
            "application/octet-stream"
        );
        assert_eq!(db.detect("empty", b""), "text/plain");
        // A cut UTF-8 sequence at the end of the sample is still text.
        assert_eq!(
            db.detect("u", "é".as_bytes().get(..1).unwrap()),
            "text/plain"
        );
    }

    #[test]
    fn globs2_weights_and_case() {
        let mut db = MimeDb::default();
        db.add_globs2(
            "# comment\n50:text/x-c:*.c\n50:text/x-csrc:*.c\n80:text/x-makefile:makefile\n\
             50:text/x-readme:README*:cs\n60:application/x-tar-gz:*.tar.gz\n50:application/gzip:*.gz\n\
             garbage line\n50:text/x-dup:*.c\n",
        );
        assert_eq!(db.detect("Makefile", b""), "text/x-makefile");
        // Longest pattern wins at equal... higher weight wins first.
        assert_eq!(db.detect("a.tar.gz", b""), "application/x-tar-gz");
        // Case-sensitive glob: README matches, readme does not.
        assert_eq!(db.detect("README.txt", b""), "text/x-readme");
        assert_eq!(db.detect("readme.txt", b"x"), "text/plain");
        // Three equal candidates and no sniff: the first wins.
        assert_eq!(db.detect("a.c", b"int x;"), "text/x-c");
    }

    #[test]
    fn magic_file_parses_and_disambiguates() {
        let mut body = rule("", 0, b"RIFF", b"");
        body.extend(rule("1", 8, b"WEBP", b""));
        let mut masked = rule("", 0, b"\xf0\x00", b"&\xf0\x00");
        masked.truncate(masked.len() - 1);
        masked.extend_from_slice(b"\n");
        let ranged = rule("", 0, b"MAGIC", b"+16");
        let file = magic_file(&[
            ("60:image/webp", &body),
            ("50:application/x-masked", &masked),
            ("40:application/x-ranged", &ranged),
        ]);
        let mut db = MimeDb::default();
        db.add_magic(&file);
        assert_eq!(db.sniff(b"RIFF\0\0\0\0WEBPxx"), Some("image/webp"));
        // RIFF without the WEBP child does not match the section.
        assert_eq!(db.sniff(b"RIFF\0\0\0\0WAVExx"), None);
        assert_eq!(db.sniff(b"\xf7\x55"), Some("application/x-masked"));
        assert_eq!(db.sniff(b"..........MAGIC"), Some("application/x-ranged"));
        assert_eq!(db.sniff(b"...................MAGIC"), None);
        // Two globs, the sniff picks between them.
        db.add_globs2("50:image/webp:*.img\n50:application/x-other:*.img\n");
        assert_eq!(db.detect("x.img", b"RIFF\0\0\0\0WEBP"), "image/webp");
    }

    #[test]
    fn malformed_magic_skips_the_section_only() {
        let good = rule("", 0, b"GOOD", b"");
        let mut file = magic_file(&[("50:application/x-bad", b"1>zz=\n")]);
        file.extend_from_slice(b"[50:application/x-good]\n");
        file.extend(good);
        let mut db = MimeDb::default();
        db.add_magic(&file);
        assert_eq!(db.sniff(b"GOOD"), Some("application/x-good"));
        let mut db = MimeDb::default();
        db.add_magic(b"not magic");
        assert!(db.magic.is_empty());
        // Truncated value: no panic, nothing kept.
        let mut db = MimeDb::default();
        db.add_magic(b"MIME-Magic\0\n[50:x/y]\n>0=\x00\x10ab");
        assert!(db.magic.is_empty());
    }

    #[test]
    fn subclasses_aliases_and_patterns() {
        let mut db = MimeDb::default();
        db.add_subclasses(
            "text/x-rust text/plain\napplication/x-shellscript application/x-executable\n",
        );
        db.add_aliases("application/x-pdf application/pdf\n");
        db.add_globs2("50:application/x-pdf:*.pdf\n");
        assert_eq!(db.detect("a.pdf", b""), "application/pdf");
        assert!(db.is_a("text/x-rust", "text/plain"));
        assert!(
            db.is_a("text/x-csrc", "text/plain"),
            "text/* is implicitly text/plain"
        );
        assert!(db.is_a("image/png", "application/octet-stream"));
        assert!(!db.is_a("inode/directory", "application/octet-stream"));
        assert!(pattern_covers("image/*", "image/png"));
        assert!(pattern_covers("IMAGE/PNG", "image/png"));
        assert!(!pattern_covers("image/*", "video/mp4"));
    }
}
