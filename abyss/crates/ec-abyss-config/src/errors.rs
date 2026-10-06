// SPDX-License-Identifier: AGPL-3.0-only
//! Config refusals: [`ConfigError`], fail-safe classification, did-you-mean
//! suggestions and the JSON/event shapes they are reported in (COMP-13 §1.2).

use super::*;

/// The one JSON shape a config refusal is reported in.
///
/// `watch::reload_now` emits it on the `config-error` event and the control
/// socket returns it from `validate_config`. One shape, one place: a GUI that
/// learns to render a hot-reload failure renders a rejected edit for free.
pub fn error_json(e: &ConfigError) -> serde_json::Value {
    serde_json::json!({
        "file": e.file.display().to_string(),
        "line": e.line,
        "col": e.col,
        "message": e.message,
        "snippet": e.snippet,
        "spanLen": e.span_len,
    })
}

/// The `config-error` event payload: every refusal, plus one line a human
/// reads at a glance.
///
/// Emitted by a failed hot reload and replayed to each new subscriber after a
/// startup that dropped nodes (ADR 0064). One event, not one per error: the
/// full list rides in `errors`, and `file`/`line`/`col`/`message` repeat the
/// first one for a consumer that shows a single problem.
pub fn error_event(errors: &[ConfigError], startup: bool) -> serde_json::Value {
    event(errors, startup, if startup { errors } else { &[] })
}

/// The `config-error` event for a failed hot reload. `live` is the refusal
/// list of the config still running: after a degraded start it carries the
/// [`FailSafe`] refusals, and while they stand the summary keeps leading with
/// them, so the fixed-id notification never trades "auto-lock is OFF" for a
/// milder "change not applied" (ADR 0064).
pub fn reload_error_event(errors: &[ConfigError], live: &[ConfigError]) -> serde_json::Value {
    event(errors, false, live)
}

pub(crate) fn event(errors: &[ConfigError], startup: bool, leads_from: &[ConfigError]) -> serde_json::Value {
    let first = errors.first();
    let mut v = serde_json::json!({
        "errors": errors.iter().map(error_json).collect::<Vec<_>>(),
        "startup": startup,
        "summary": summary(errors, startup, leads_from),
    });
    if let Some(e) = first {
        v["file"] = e.file.display().to_string().into();
        v["line"] = e.line.into();
        v["col"] = e.col.into();
        v["message"] = e.message.clone().into();
    }
    v
}

/// `abyss.kdl: 1 problem ignored — line 14: touchpad key needs a boolean: "drag-lock"`.
///
/// At startup a refusal that left a protection off ([`FailSafe`]) leads,
/// named for what it switched off — `abyss.kdl: auto-lock is OFF — line 3: …`
/// — so that "(and N more)" can never be where it hides.
///
/// After a failed reload the leads come from the *live* config (`leads_from`)
/// and go before the reload's own part:
/// `abyss.kdl: auto-lock is OFF — line 6: …; 1 problem, change not applied — line 2: …`.
pub(crate) fn summary(errors: &[ConfigError], startup: bool, leads_from: &[ConfigError]) -> String {
    let Some(first) = errors.first() else {
        return String::new();
    };
    let file = first
        .file
        .file_name()
        .map_or_else(|| "config".to_string(), |n| n.to_string_lossy().into_owned());
    let n = errors.len();
    let at = |e: &ConfigError| {
        if e.line > 0 {
            format!("line {}: {}", e.line, e.message)
        } else {
            e.message.clone()
        }
    };
    let leads: Vec<String> = [FailSafe::AutoLockOff, FailSafe::XwaylandOff]
        .into_iter()
        .filter_map(|g| {
            leads_from
                .iter()
                .find(|e| e.fail_safe == Some(g))
                .map(|e| format!("{} \u{2014} {}", g.label(), at(e)))
        })
        .collect();
    let plural = if n == 1 { "" } else { "s" };
    let (mut s, shown) = if startup && !leads.is_empty() {
        (format!("{file}: {}", leads.join("; ")), leads.len())
    } else if startup {
        (
            format!("{file}: {n} problem{plural} ignored \u{2014} {}", at(first)),
            1,
        )
    } else {
        let body = format!("{n} problem{plural}, change not applied \u{2014} {}", at(first));
        if leads.is_empty() {
            (format!("{file}: {body}"), 1)
        } else {
            (format!("{file}: {}; {body}", leads.join("; ")), 1)
        }
    };
    if n > shown {
        s.push_str(&format!(
            " (and {} more; `ec-ctl config validate` lists them)",
            n - shown
        ));
    }
    s
}

/// A refusal from config validation (COMP-13 §1.2). Carries the precise
/// `file:line:col` the spec requires so the message can be acted on directly.
#[derive(Debug, Clone)]
pub struct ConfigError {
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
    pub message: String,
    /// The offending source line, verbatim, and how many columns of it the
    /// token covers — so [`Display`] can point at it the way rustc does.
    pub snippet: Option<String>,
    pub span_len: usize,
    /// True when dropping this node at startup would leave a default that is
    /// not safe, so Abyss refuses to start rather than ignore it (ADR 0064):
    /// anything in `policy.kdl`, a policy-owned key in `abyss.kdl`, and
    /// `misc.render-device`.
    pub startup_fatal: bool,
    /// Set when this refusal is evidence that the owner tried to configure a
    /// protection and it did not take (ADR 0064). Startup still starts, but
    /// the summary leads with it, and for Xwayland the safe value (off) is
    /// used instead of the default. See [`Config::startup`].
    pub fail_safe: Option<FailSafe>,
}

/// A protection a dropped node may have been meant to configure (ADR 0064).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailSafe {
    /// A refused lock setting: the built-in default (never auto-lock) stays.
    /// Owner decision 2026-09-25: start anyway and say so plainly.
    AutoLockOff,
    /// A refused `xwayland` setting: Xwayland is forced off, the isolating
    /// value (ADR 0026), rather than the `enable #true` default.
    XwaylandOff,
}

impl FailSafe {
    fn label(self) -> &'static str {
        match self {
            FailSafe::AutoLockOff => "auto-lock is OFF",
            FailSafe::XwaylandOff => "Xwayland is OFF",
        }
    }
}

/// Which protection an *unknown top-level node* was evidently meant to be:
/// within edit distance 2 of `idle` or `xwayland` (`idel`, `xwyland`). Simple
/// and deterministic on purpose; a false hit only makes the notice louder
/// (and, for `xwayland`, starts without X11).
pub(crate) fn misspelt_guard(name: &str) -> Option<FailSafe> {
    if edit_distance(name, "idle") <= 2 {
        Some(FailSafe::AutoLockOff)
    } else if edit_distance(name, "xwayland") <= 2 {
        Some(FailSafe::XwaylandOff)
    } else {
        None
    }
}

/// What startup does with a loaded config (COMP-01 §5 step 3, amended by
/// ADR 0064).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    /// Start. `ignored` rejected nodes were dropped; each setting they would
    /// have set keeps its default (or a lower-precedence file's value).
    Start { ignored: usize },
    /// At least one refusal is in the fail-closed set: do not start.
    Refuse { fatal: usize },
}

/// Nearest schema path by edit distance, when it is near enough to be worth
/// suggesting. Bounded at a third of the name's length so a wholly different
/// word never gets proposed as a typo.
pub(crate) fn did_you_mean(path: &str) -> Option<&'static str> {
    let budget = (path.len() / 3).max(1);
    schema::TABLE
        .iter()
        .map(|k| (edit_distance(path, k.path), k.path))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, _)| *d)
        .map(|(_, p)| p)
}

/// [`did_you_mean`] over any candidate list: the nearest within a third of
/// `word`'s length, or nothing.
pub(crate) fn nearest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let budget = (word.len() / 3).max(1);
    candidates
        .into_iter()
        .map(|c| (edit_distance(word, c), c))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Locate `offset` in `text`: 1-based line and column, plus the whole line it
/// falls on. `len` is clamped to what is left of that line so the caret run
/// never spills past the snippet.
pub(crate) fn locate(text: &str, offset: usize, len: usize) -> (usize, usize, String, usize) {
    let off = offset.min(text.len());
    let start = text[..off].rfind('\n').map_or(0, |i| i + 1);
    let end = text[off..].find('\n').map_or(text.len(), |i| off + i);
    let line = text[..off].matches('\n').count() + 1;
    (
        line,
        off - start + 1,
        text[start..end].to_string(),
        len.clamp(1, end - off),
    )
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.file.display(),
            self.line,
            self.col,
            self.message
        )?;
        // Show the line and underline the token, so the position is actionable
        // without opening the file (COMP-13 §1.2 "the offending token").
        if let Some(src) = &self.snippet {
            let n = self.line.to_string();
            let pad = " ".repeat(n.len());
            write!(f, "\n{pad} |\n{n} | {src}\n{pad} | ")?;
            for (i, c) in src.char_indices() {
                if i + 1 >= self.col {
                    break;
                }
                // Keep tabs as tabs so the caret lines up in the user's terminal.
                f.write_str(if c == '\t' { "\t" } else { " " })?;
            }
            write!(f, "{}", "^".repeat(self.span_len.max(1)))?;
        }
        Ok(())
    }
}
