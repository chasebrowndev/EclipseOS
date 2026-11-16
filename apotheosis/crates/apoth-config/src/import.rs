// SPDX-License-Identifier: AGPL-3.0-only
//! One-shot importers (T-10 §6). The foot importer is R1: the default switch (T-24) runs it
//! for existing users.
//!
//! `import_foot` turns `foot.ini` text into `apotheosis.kdl` text through the lossless
//! [`Editor`], so every emitted value passed the same validation a load applies. Every foot
//! key yields exactly one [`ReportEntry`] (a key may also appear in `targets` of a later
//! synthetic entry, e.g. the palette): mapped, approximate (mapped with a stated difference)
//! or unmapped with a reason. Nothing is dropped silently. Values are never copied into
//! report notes.

use crate::edit::Editor;
use crate::fs::MAX_FILE_BYTES;
use crate::lang::{format_color, parse_color};
use crate::types::Value;

/// How well a foot key translated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Same meaning.
    Mapped,
    /// Written, but the meaning differs in a way `note` states.
    Approximate,
    /// Not written; `note` says why.
    Unmapped,
}

/// One input line and what became of it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportEntry {
    pub section: String,
    pub key: String,
    /// 1-based line in the foot file; 0 for synthetic entries.
    pub line: usize,
    pub status: Status,
    /// The apotheosis keys written for it.
    pub targets: Vec<&'static str>,
    pub note: String,
}

/// The translation and its report.
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub kdl: String,
    pub report: Vec<ReportEntry>,
}

impl Import {
    pub fn unmapped(&self) -> impl Iterator<Item = &ReportEntry> {
        self.report.iter().filter(|e| e.status == Status::Unmapped)
    }

    pub fn approximate(&self) -> impl Iterator<Item = &ReportEntry> {
        self.report
            .iter()
            .filter(|e| e.status == Status::Approximate)
    }

    /// Human-readable report: one line per entry that is not a clean mapping, then counts.
    pub fn render_report(&self) -> String {
        let mut out = String::new();
        for e in &self.report {
            let tag = match e.status {
                Status::Mapped => continue,
                Status::Approximate => "approximate",
                Status::Unmapped => "not imported",
            };
            let pos = if e.line > 0 {
                format!("line {}: ", e.line)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "{pos}[{}] {}: {tag}: {}\n",
                e.section, e.key, e.note
            ));
        }
        let n = |s: Status| self.report.iter().filter(|e| e.status == s).count();
        out.push_str(&format!(
            "{} mapped, {} approximate, {} not imported\n",
            n(Status::Mapped),
            n(Status::Approximate),
            n(Status::Unmapped)
        ));
        out
    }
}

struct Raw<'a> {
    section: &'a str,
    key: &'a str,
    value: &'a str,
    line: usize,
}

/// xterm's default 16 colours; fills palette slots the foot file leaves unset.
const FILL: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x00, 0x00],
    [0x00, 0xcd, 0x00],
    [0xcd, 0xcd, 0x00],
    [0x00, 0x00, 0xee],
    [0xcd, 0x00, 0xcd],
    [0x00, 0xcd, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x7f, 0x7f, 0x7f],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x5c, 0x5c, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

struct Imp {
    ed: Editor,
    report: Vec<ReportEntry>,
    pal: [Option<[u8; 4]>; 16],
    pal_entries: Vec<usize>,
}

fn yn(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => Some(true),
        "no" | "false" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn hex(s: &str) -> Option<[u8; 4]> {
    parse_color(&format!("#{}", s.trim().trim_start_matches('#')))
}

/// Split on whitespace honouring single and double quotes and backslash escapes.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (_, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    has = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (_, c) => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// `family:size=11:weight=bold` entries separated by commas: `(family, attributes)`.
fn parse_fonts(s: &str) -> Vec<(String, Vec<(String, String)>)> {
    s.split(',')
        .filter_map(|part| {
            let mut it = part.split(':');
            let fam = it.next()?.trim().to_string();
            if fam.is_empty() {
                return None;
            }
            let attrs = it
                .filter_map(|a| {
                    let (k, v) = a.split_once('=')?;
                    Some((k.trim().to_string(), v.trim().to_string()))
                })
                .collect();
            Some((fam, attrs))
        })
        .collect()
}

/// `N`, `Npx`, `-N`: the adjust form `Npx`. Anything with another unit is `None`.
fn px_adjust(s: &str) -> Option<String> {
    let t = s.trim();
    let n = t.strip_suffix("px").unwrap_or(t).trim();
    let body = n.strip_prefix(['+', '-']).unwrap_or(n);
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    Some(format!("{n}px"))
}

impl Imp {
    fn entry(&mut self, r: &Raw, status: Status, targets: &[&'static str], note: &str) {
        self.report.push(ReportEntry {
            section: r.section.to_string(),
            key: r.key.to_string(),
            line: r.line,
            status,
            targets: targets.to_vec(),
            note: note.to_string(),
        });
    }

    fn unmapped(&mut self, r: &Raw, note: &str) {
        self.entry(r, Status::Unmapped, &[], note);
    }

    fn put(&mut self, r: &Raw, path: &'static str, v: Value, status: Status, note: &str) {
        match self.ed.set(path, &v) {
            Ok(()) => self.entry(r, status, &[path], note),
            Err(e) => self.unmapped(r, &format!("rejected by `{path}`: {e}")),
        }
    }

    fn put_bool(&mut self, r: &Raw, path: &'static str, invert: bool) {
        match yn(r.value) {
            Some(b) => self.put(r, path, Value::Bool(b != invert), Status::Mapped, ""),
            None => self.unmapped(r, "expected yes or no"),
        }
    }

    fn put_str(&mut self, r: &Raw, path: &'static str) {
        self.put(r, path, Value::Str(r.value.to_string()), Status::Mapped, "");
    }

    fn put_color(&mut self, r: &Raw, path: &'static str) {
        match hex(r.value) {
            Some(c) => self.put(r, path, Value::Color(c), Status::Mapped, ""),
            None => self.unmapped(r, "expected a hex colour such as 1d2021"),
        }
    }

    fn put_num(&mut self, r: &Raw, path: &'static str, int: bool) {
        let v = r.value.trim();
        if int {
            match v.parse::<i64>() {
                Ok(i) => self.put(r, path, Value::Int(i), Status::Mapped, ""),
                Err(_) => self.unmapped(r, "expected an integer"),
            }
        } else {
            match v.parse::<f64>() {
                Ok(f) => self.put(r, path, Value::Float(f), Status::Mapped, ""),
                Err(_) => self.unmapped(r, "expected a number"),
            }
        }
    }

    fn put_font_slot(&mut self, r: &Raw, path: &'static str) {
        let fonts = parse_fonts(r.value);
        let Some((fam, attrs)) = fonts.into_iter().next() else {
            return self.unmapped(r, "empty font specification");
        };
        let note = if attrs.is_empty() {
            ""
        } else {
            "font attributes (size, style, weight) dropped; only the family is used"
        };
        let st = if attrs.is_empty() {
            Status::Mapped
        } else {
            Status::Approximate
        };
        self.put(r, path, Value::Str(fam), st, note);
    }

    fn main_font(&mut self, r: &Raw) {
        let fonts = parse_fonts(r.value);
        let mut it = fonts.into_iter();
        let Some((fam, attrs)) = it.next() else {
            return self.unmapped(r, "empty font specification");
        };
        let mut notes: Vec<String> = Vec::new();
        let mut size: Option<f64> = None;
        for (k, v) in &attrs {
            match (k.as_str(), v.parse::<f64>()) {
                ("size", Ok(s)) => size = Some(s),
                ("pixelsize", _) => {
                    notes.push("pixelsize has no equivalent (size is in points)".into())
                }
                (name, _) => notes.push(format!("attribute `{name}` dropped")),
            }
        }
        let fallback: Vec<String> = it.map(|(f, _)| f).collect();
        let mut targets: Vec<&'static str> = Vec::new();
        let mut errs: Vec<String> = Vec::new();
        let mut try_set = |ed: &mut Editor, path: &'static str, v: Value| match ed.set(path, &v) {
            Ok(()) => targets.push(path),
            Err(e) => errs.push(format!("rejected by `{path}`: {e}")),
        };
        try_set(&mut self.ed, "font.family", Value::Str(fam));
        if let Some(s) = size {
            try_set(&mut self.ed, "font.size", Value::Float(s));
        }
        if !fallback.is_empty() {
            try_set(&mut self.ed, "font.fallback", Value::List(fallback));
        }
        notes.extend(errs.iter().cloned());
        let status = if targets.is_empty() {
            Status::Unmapped
        } else if notes.is_empty() {
            Status::Mapped
        } else {
            Status::Approximate
        };
        self.entry(r, status, &targets, &notes.join("; "));
    }

    fn map(&mut self, r: &Raw) {
        let sec = match r.section {
            "colors-dark" => "colors",
            s => s,
        };
        match (sec, r.key) {
            ("main", "term") => self.put_str(r, "terminal.term"),
            ("main", "shell") => {
                let t = tokenize(r.value);
                let Some((prog, args)) = t.split_first() else {
                    return self.unmapped(r, "empty shell command");
                };
                let mut targets = vec!["shell.program"];
                if let Err(e) = self.ed.set("shell.program", &Value::Str(prog.clone())) {
                    return self.unmapped(r, &format!("rejected by `shell.program`: {e}"));
                }
                let mut note = String::new();
                if !args.is_empty() {
                    match self.ed.set("shell.args", &Value::List(args.to_vec())) {
                        Ok(()) => targets.push("shell.args"),
                        Err(e) => note = format!("arguments rejected by `shell.args`: {e}"),
                    }
                }
                let st = if note.is_empty() {
                    Status::Mapped
                } else {
                    Status::Approximate
                };
                self.entry(r, st, &targets, &note);
            }
            ("main", "login-shell") => self.put_bool(r, "shell.login", false),
            ("main", "title") => self.put_str(r, "window.title"),
            ("main", "pad") => {
                let mut it = r.value.split_whitespace();
                let dims = it.next().and_then(|d| d.split_once('x'));
                let parsed = dims.and_then(|(x, y)| Some((x.parse::<i64>().ok()?, y.parse::<i64>().ok()?)));
                let center = it.next() == Some("center");
                let Some((x, y)) = parsed else {
                    return self.unmapped(r, "expected XxY pixels");
                };
                let mut targets = vec!["window.padding-x", "window.padding-y"];
                let mut note = String::new();
                for (p, v) in [("window.padding-x", x), ("window.padding-y", y)] {
                    if let Err(e) = self.ed.set(p, &Value::Int(v)) {
                        note = format!("rejected by `{p}`: {e}");
                    }
                }
                if center {
                    match self.ed.set("window.padding-balance", &Value::Bool(true)) {
                        Ok(()) => targets.push("window.padding-balance"),
                        Err(e) => note = format!("rejected by `window.padding-balance`: {e}"),
                    }
                }
                let st = if note.is_empty() {
                    Status::Mapped
                } else {
                    Status::Approximate
                };
                self.entry(r, st, &targets, &note);
            }
            ("main", "resize-by-cells") => self.put_bool(r, "window.resize-by-cells", false),
            ("main", "bold-text-in-bright") => match r.value.trim() {
                "palette-based" => self.put(
                    r,
                    "colors.bold-is-bright",
                    Value::Bool(true),
                    Status::Approximate,
                    "palette-based brightening becomes plain bold-is-bright",
                ),
                _ => self.put_bool(r, "colors.bold-is-bright", false),
            },
            ("main", "word-delimiters") => self.put_str(r, "selection.word-delimiters"),
            ("main", "selection-target") => match r.value.trim() {
                "none" => self.put(r, "selection.copy-on-select", Value::Bool(false), Status::Mapped, ""),
                "primary" => self.put(r, "selection.copy-on-select", Value::Bool(true), Status::Mapped, ""),
                "clipboard" | "both" => self.put(
                    r,
                    "selection.copy-on-select",
                    Value::Bool(true),
                    Status::Approximate,
                    "copy-on-select follows the primary-selection setting, not a clipboard choice",
                ),
                _ => self.unmapped(r, "expected none, primary, clipboard or both"),
            },
            ("main", "font") => self.main_font(r),
            ("main", "font-bold") => self.put_font_slot(r, "font.bold-family"),
            ("main", "font-italic") => self.put_font_slot(r, "font.italic-family"),
            ("main", "font-bold-italic") => self.put_font_slot(r, "font.bold-italic-family"),
            ("main", "letter-spacing") => match px_adjust(r.value) {
                Some(a) => self.put(r, "font.adjust-cell-width", Value::Str(a), Status::Approximate, "foot adds the spacing to the advance; apotheosis adjusts the cell width"),
                None => self.unmapped(r, "only pixel values can be expressed (use px)"),
            },
            ("main", "underline-offset") => match px_adjust(r.value) {
                Some(a) => self.put(r, "font.adjust-underline-position", Value::Str(a), Status::Approximate, "foot overrides the position; apotheosis adjusts the font's"),
                None => self.unmapped(r, "only pixel values can be expressed (use px)"),
            },
            ("main", "underline-thickness") => match px_adjust(r.value) {
                Some(a) => self.put(r, "font.adjust-underline-thickness", Value::Str(a), Status::Approximate, "foot overrides the thickness; apotheosis adjusts the font's"),
                None => self.unmapped(r, "only pixel values can be expressed (use px)"),
            },
            ("main", "strikeout-thickness") => match px_adjust(r.value) {
                Some(a) => self.put(r, "font.adjust-strikethrough-thickness", Value::Str(a), Status::Approximate, "foot overrides the thickness; apotheosis adjusts the font's"),
                None => self.unmapped(r, "only pixel values can be expressed (use px)"),
            },
            ("main", "box-drawings-uses-font-glyphs") => {
                self.put_bool(r, "font.builtin-glyphs.box", true)
            }
            ("main", "gamma-correct-blending") => match yn(r.value) {
                Some(b) => self.put(
                    r,
                    "render.text-blending",
                    Value::Str(if b { "linear-corrected" } else { "native" }.into()),
                    Status::Approximate,
                    "blending mode names differ",
                ),
                None => self.unmapped(r, "expected yes or no"),
            },
            ("main", "line-height") => self.unmapped(
                r,
                "foot sets an absolute line height; apotheosis only adjusts it (font.adjust-cell-height)",
            ),
            ("main", "font-size-adjustment") => {
                self.unmapped(r, "zoom step is `font.zoom-step`; set it by hand if wanted")
            }
            ("main", "include") => self.unmapped(r, "import the included file separately, then `include` it"),
            ("bell", "urgent") => self.put_bool(r, "bell.urgent", false),
            ("bell", "notify") => self.put_bool(r, "bell.notify", false),
            ("bell", "visual") => self.put_bool(r, "bell.visual", false),
            ("bell", "command") => {
                let t = tokenize(&r.value.replace("${", "{"));
                self.put(r, "bell.command", Value::List(t), Status::Mapped, "");
            }
            ("desktop-notifications", "inhibit-when-focused") => match yn(r.value) {
                Some(b) => self.put(
                    r,
                    "notifications.when",
                    Value::Str(if b { "unfocused" } else { "always" }.into()),
                    Status::Approximate,
                    "applies to terminal notifications (OSC 9/99/777)",
                ),
                None => self.unmapped(r, "expected yes or no"),
            },
            ("scrollback", "lines") => self.put_num(r, "scrollback.lines", true),
            ("scrollback", "multiplier") => self.put_num(r, "mouse.scroll-multiplier", false),
            ("url", "launch") => {
                let t = tokenize(&r.value.replace("${url}", "{url}"));
                self.put(r, "links.open-command", Value::List(t), Status::Mapped, "");
            }
            ("url", "label-letters") => self.put_str(r, "hints.alphabet"),
            ("url", "protocols") => {
                let list: Vec<String> = r
                    .value
                    .split(',')
                    .map(|s| s.trim().trim_end_matches("://").to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                self.put(r, "links.schemes", Value::List(list), Status::Mapped, "");
            }
            ("cursor", "style") => {
                let v = match r.value.trim() {
                    "block" => "block",
                    "beam" => "bar",
                    "underline" => "underline",
                    _ => return self.unmapped(r, "expected block, beam or underline"),
                };
                self.put(r, "cursor.style", Value::Str(v.into()), Status::Mapped, "");
            }
            ("cursor", "blink") => self.put_bool(r, "cursor.blink", false),
            ("cursor", "blink-rate") => match r.value.trim().parse::<i64>() {
                Ok(ms) => self.put(r, "cursor.blink-interval", Value::Int(ms), Status::Mapped, ""),
                Err(_) => self.unmapped(r, "expected milliseconds"),
            },
            ("cursor", "color") => {
                let mut it = r.value.split_whitespace();
                let (text, cur) = (it.next().and_then(hex), it.next().and_then(hex));
                let (Some(text), Some(cur)) = (text, cur) else {
                    return self.unmapped(r, "expected two hex colours: text cursor");
                };
                let a = self.ed.set("colors.cursor-text", &Value::Color(text));
                let b = self.ed.set("colors.cursor", &Value::Color(cur));
                match (a, b) {
                    (Ok(()), Ok(())) => self.entry(r, Status::Mapped, &["colors.cursor-text", "colors.cursor"], ""),
                    (a, b) => {
                        let e = a.err().or(b.err()).map(|e| e.to_string()).unwrap_or_default();
                        self.unmapped(r, &format!("rejected: {e}"))
                    }
                }
            }
            ("mouse", "hide-when-typing") => self.put_bool(r, "mouse.hide-while-typing", false),
            ("mouse", "alternate-scroll-mode") => self.put_bool(r, "mouse.alternate-scroll-mode", false),
            ("csd", "preferred") => match r.value.trim() {
                v @ ("server" | "client" | "none") => {
                    self.put(r, "window.decorations", Value::Str(v.into()), Status::Mapped, "")
                }
                _ => self.unmapped(r, "expected server, client or none"),
            },
            ("security", "osc52") => match r.value.trim() {
                "disabled" => self.put(r, "clipboard.osc52", Value::Bool(false), Status::Mapped, ""),
                "enabled" => self.put(r, "clipboard.osc52", Value::Bool(true), Status::Mapped, ""),
                "copy-enabled" | "paste-enabled" => self.put(
                    r,
                    "clipboard.osc52",
                    Value::Bool(true),
                    Status::Approximate,
                    "apotheosis has one switch; the one-direction restriction is lost (paste is gated by `paste` and the consent prompt)",
                ),
                _ => self.unmapped(r, "unknown osc52 mode"),
            },
            ("colors", "alpha") => self.put_num(r, "window.opacity", false),
            ("colors", "foreground") => self.put_color(r, "colors.foreground"),
            ("colors", "background") => self.put_color(r, "colors.background"),
            ("colors", "selection-foreground") => self.put_color(r, "colors.selection-foreground"),
            ("colors", "selection-background") => self.put_color(r, "colors.selection-background"),
            ("colors", "urls") => self.put_color(r, "colors.link"),
            ("colors", k) if palette_slot(k).is_some() => {
                let i = palette_slot(k).unwrap_or(0);
                match hex(r.value) {
                    Some(c) => {
                        self.pal[i] = Some(c);
                        self.pal_entries.push(self.report.len());
                        self.entry(r, Status::Mapped, &["colors.palette"], "");
                    }
                    None => self.unmapped(r, "expected a hex colour"),
                }
            }
            ("key-bindings" | "search-bindings" | "url-bindings" | "text-bindings", _) => {
                self.unmapped(r, "foot bindings need manual translation to `binds` (T-11)")
            }
            ("mouse-bindings", _) => {
                self.unmapped(r, "foot mouse bindings need manual translation to `mouse-binds`")
            }
            ("tweak", _) => self.unmapped(r, "foot tweak options have no equivalent"),
            ("colors-light", _) => self.unmapped(
                r,
                "light variant: choose a scheme pair with `theme light=… dark=…`",
            ),
            (
                "main" | "bell" | "desktop-notifications" | "scrollback" | "url" | "cursor"
                | "mouse" | "csd" | "security" | "colors",
                _,
            ) => self.unmapped(r, "no apotheosis equivalent"),
            _ => self.unmapped(r, "unknown section; no apotheosis equivalent"),
        }
    }

    fn finish_palette(&mut self) {
        if self.pal.iter().all(Option::is_none) {
            return;
        }
        let missing: Vec<usize> = (0..16).filter(|i| self.pal[*i].is_none()).collect();
        let list: Vec<String> = (0..16)
            .map(|i| {
                let c = self.pal[i].unwrap_or([FILL[i][0], FILL[i][1], FILL[i][2], 255]);
                format_color(c)
            })
            .collect();
        match self.ed.set("colors.palette", &Value::List(list)) {
            Ok(()) => {
                if !missing.is_empty() {
                    let slots = missing
                        .iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.report.push(ReportEntry {
                        section: "colors".into(),
                        key: "palette".into(),
                        line: 0,
                        status: Status::Approximate,
                        targets: vec!["colors.palette"],
                        note: format!(
                            "apotheosis needs all 16 colours; slots {slots} were filled with xterm defaults"
                        ),
                    });
                }
            }
            Err(e) => {
                for i in std::mem::take(&mut self.pal_entries) {
                    self.report[i].status = Status::Unmapped;
                    self.report[i].targets.clear();
                    self.report[i].note = format!("palette rejected: {e}");
                }
            }
        }
    }
}

/// `regular0..7` -> 0..7, `bright0..7` -> 8..15.
fn palette_slot(k: &str) -> Option<usize> {
    let (base, rest) = if let Some(r) = k.strip_prefix("regular") {
        (0, r)
    } else {
        (8, k.strip_prefix("bright")?)
    };
    let n: usize = rest.parse().ok()?;
    (n < 8).then_some(base + n)
}

/// Translate `foot.ini` text. Never fails: whatever cannot be translated is in the report.
pub fn import_foot(text: &str) -> Import {
    let mut imp = Imp {
        ed: Editor::parse("// Imported from foot.ini\nversion 1\n")
            .expect("fixed document is valid KDL"),
        report: Vec::new(),
        pal: [None; 16],
        pal_entries: Vec::new(),
    };
    if text.len() as u64 > MAX_FILE_BYTES {
        imp.report.push(ReportEntry {
            section: String::new(),
            key: String::new(),
            line: 0,
            status: Status::Unmapped,
            targets: Vec::new(),
            note: "file larger than 1 MiB; nothing imported".into(),
        });
        return Import {
            kdl: imp.ed.text(),
            report: imp.report,
        };
    }
    let mut section = "main".to_string();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(name) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        let Some((k, v)) = t.split_once('=') else {
            imp.report.push(ReportEntry {
                section: section.clone(),
                key: String::new(),
                line,
                status: Status::Unmapped,
                targets: Vec::new(),
                note: "not a key=value line".into(),
            });
            continue;
        };
        let r = Raw {
            section: &section,
            key: k.trim(),
            value: v.trim(),
            line,
        };
        imp.map(&r);
    }
    imp.finish_palette();
    Import {
        kdl: imp.ed.text(),
        report: imp.report,
    }
}
