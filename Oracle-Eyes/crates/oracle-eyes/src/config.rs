// SPDX-License-Identifier: AGPL-3.0-only

//! KDL configuration for the daemon (spec §5).
//!
//! Search path, later files overriding earlier ones:
//!   1. `/etc/eclipse/oracle-eyes.kdl`
//!   2. `$XDG_CONFIG_HOME/eclipse/oracle-eyes.kdl` (or `~/.config/...`)
//!
//! Same syntax family as the compositor's config, and the same refusal
//! discipline (`abyss/src/config/mod.rs`): an unknown key or a malformed
//! value is an error carrying `file:line:col`, never a warning and never a
//! silent skip. [`load`] returns the errors alongside a usable config rather
//! than a bare `Result`, because the two halves are independent: one bad key
//! must not cost the user the other fifteen, and "fail visibly" (CLAUDE.md)
//! means the caller still has to show what it refused. Startup prints them;
//! a future hot-reload can keep the last good config and report the rest.
//!
//! Every number in spec §5 lives here. None of them may harden into a
//! constant elsewhere in the crate.

use std::fmt;
use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode};

/// A refusal from config validation. Position is the offending token's, so
/// the message can be acted on without opening the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.file.display(),
            self.line,
            self.col,
            self.message
        )
    }
}

/// Every tunable in spec §5, plus the two binaries the pipeline shells out
/// to. Defaults are the §5 table verbatim.
#[derive(Debug, Clone)]
pub struct Config {
    /// Damage must go quiet this long before a region counts as settled.
    pub settle_ms: u64,
    /// Floor and ceiling of the §2.2 display-time formula, and its slope.
    pub min_display_ms: u64,
    pub ms_per_word: u64,
    pub max_display_ms: u64,
    /// How long the select-mode failure indicator stays up (§2.1).
    pub fail_indicator_ms: u64,
    /// Minimum gap between dispatched queries (§3.3): 1 per 3s.
    pub rate_limit_ms: u64,
    /// How long an already-answered region hash suppresses a re-ask (§3.3).
    /// Inputs only — no answer is ever stored.
    pub dedup_ttl_ms: u64,
    /// Words before a `?` for the fast path to fire (§3.3).
    pub fast_path_min_words: usize,
    /// The model call's ceiling. Not in the §5 table, but hardcoding it
    /// would make the one unbounded wait in the pipeline unconfigurable.
    pub answer_timeout_ms: u64,
    /// Answer length contract (§3.4). `word_cap` is what the system prompt
    /// asks for; `char_cap` is the backstop that does not depend on the
    /// model cooperating.
    pub word_cap: usize,
    pub char_cap: usize,
    /// Automatic mode (§2.2). Off by default: it spends queries and reads
    /// the whole screen, so it is opt-in.
    pub auto: bool,
    pub tesseract_bin: String,
    /// Tesseract language pack. Config rather than a constant because the
    /// answer is only as good as the OCR, and the right pack is the user's.
    pub ocr_lang: String,
    pub claude_bin: String,
    /// `--model` for the CLI. `None` leaves the CLI's own default alone
    /// rather than pinning a name that may stop existing.
    pub model: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            settle_ms: 600,
            min_display_ms: 4000,
            ms_per_word: 150,
            max_display_ms: 20000,
            fail_indicator_ms: 1500,
            rate_limit_ms: 3000,
            dedup_ttl_ms: 5 * 60 * 1000,
            fast_path_min_words: 3,
            answer_timeout_ms: 30000,
            word_cap: 60,
            char_cap: 400,
            auto: false,
            tesseract_bin: "tesseract".to_string(),
            ocr_lang: "eng".to_string(),
            claude_bin: "claude".to_string(),
            model: None,
        }
    }
}

/// System file first so the user's own file wins on any key it names.
fn search_path() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/etc/eclipse/oracle-eyes.kdl")];
    let home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(dir) = home {
        v.push(dir.join("eclipse/oracle-eyes.kdl"));
    }
    v
}

/// Load the search path. A missing file is the normal case and contributes
/// nothing; an unreadable or malformed one is reported.
pub fn load() -> (Config, Vec<ConfigError>) {
    let mut cfg = Config::default();
    let mut errors = Vec::new();
    for path in search_path() {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            // Absent is ordinary; anything else the owner wanted to work.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                errors.push(ConfigError {
                    file: path.clone(),
                    line: 0,
                    col: 0,
                    message: format!("config unreadable: {e}"),
                });
                continue;
            }
        };
        apply_text(&mut cfg, &path, &text, &mut errors);
    }
    (cfg, errors)
}

/// Parse one document over `cfg`. Split out so the tests exercise exactly
/// what `load` does without touching the filesystem.
pub fn apply_text(cfg: &mut Config, path: &Path, text: &str, errors: &mut Vec<ConfigError>) {
    let doc = match text.parse::<KdlDocument>() {
        Ok(d) => d,
        Err(e) => {
            // The first diagnostic carries the position and the "expected"
            // help; the outer error is generic.
            let (off, message) = match e.diagnostics.first() {
                Some(d) => (
                    d.span.offset(),
                    d.message
                        .clone()
                        .unwrap_or_else(|| "invalid syntax".to_string()),
                ),
                None => (0, e.to_string()),
            };
            let (line, col) = locate(text, off);
            errors.push(ConfigError {
                file: path.to_path_buf(),
                line,
                col,
                message,
            });
            return;
        }
    };
    let mut w = Walk {
        file: path,
        text,
        errors,
    };
    for node in doc.nodes() {
        match node.name().value() {
            "timing" => w.section(node, cfg, timing_key),
            "query" => w.section(node, cfg, query_key),
            "answer" => w.section(node, cfg, answer_key),
            "binaries" => w.section(node, cfg, binaries_key),
            "auto" => {
                if let Some(v) = w.boolean(node) {
                    cfg.auto = v;
                }
            }
            "ocr-lang" => {
                if let Some(v) = w.string(node) {
                    cfg.ocr_lang = v;
                }
            }
            "model" => {
                if let Some(v) = w.string(node) {
                    cfg.model = Some(v);
                }
            }
            other => w.reject(node, format!("unknown node {other:?}")),
        }
    }
}

/// Section handlers. Each returns false for a name it does not own, which
/// is how an unknown key becomes a reported error instead of a no-op.
type KeyFn = fn(&mut Walk<'_>, &KdlNode, &str, &mut Config) -> bool;

fn timing_key(w: &mut Walk<'_>, node: &KdlNode, key: &str, cfg: &mut Config) -> bool {
    match key {
        "settle_ms" => w.set_ms(node, &mut cfg.settle_ms),
        "min_display_ms" => w.set_ms(node, &mut cfg.min_display_ms),
        "ms_per_word" => w.set_ms(node, &mut cfg.ms_per_word),
        "max_display_ms" => w.set_ms(node, &mut cfg.max_display_ms),
        "fail_indicator_ms" => w.set_ms(node, &mut cfg.fail_indicator_ms),
        _ => return false,
    }
    true
}

fn query_key(w: &mut Walk<'_>, node: &KdlNode, key: &str, cfg: &mut Config) -> bool {
    match key {
        "rate_limit_ms" => w.set_ms(node, &mut cfg.rate_limit_ms),
        "dedup_ttl_ms" => w.set_ms(node, &mut cfg.dedup_ttl_ms),
        "timeout_ms" => w.set_ms(node, &mut cfg.answer_timeout_ms),
        "fast_path_min_words" => w.set_count(node, &mut cfg.fast_path_min_words),
        _ => return false,
    }
    true
}

fn answer_key(w: &mut Walk<'_>, node: &KdlNode, key: &str, cfg: &mut Config) -> bool {
    match key {
        "word_cap" => w.set_count(node, &mut cfg.word_cap),
        "char_cap" => w.set_count(node, &mut cfg.char_cap),
        _ => return false,
    }
    true
}

fn binaries_key(w: &mut Walk<'_>, node: &KdlNode, key: &str, cfg: &mut Config) -> bool {
    match key {
        "tesseract" => {
            if let Some(v) = w.string(node) {
                cfg.tesseract_bin = v;
            }
        }
        "claude" => {
            if let Some(v) = w.string(node) {
                cfg.claude_bin = v;
            }
        }
        _ => return false,
    }
    true
}

/// Parse state: what is needed to turn a node into a positioned refusal.
struct Walk<'a> {
    file: &'a Path,
    text: &'a str,
    errors: &'a mut Vec<ConfigError>,
}

impl Walk<'_> {
    fn section(&mut self, node: &KdlNode, cfg: &mut Config, key: KeyFn) {
        let Some(children) = node.children() else {
            self.reject(node, format!("{:?} has no settings", node.name().value()));
            return;
        };
        for child in children.nodes() {
            let name = child.name().value().to_string();
            if !key(self, child, &name, cfg) {
                self.reject(
                    child,
                    format!("unknown key {:?} in {:?}", name, node.name().value()),
                );
            }
        }
    }

    /// Durations are unsigned: a negative settle time is a typo with a
    /// plausible-looking value, which is exactly the case worth catching.
    fn set_ms(&mut self, node: &KdlNode, slot: &mut u64) {
        if let Some(v) = self.integer(node) {
            *slot = v;
        }
    }

    fn set_count(&mut self, node: &KdlNode, slot: &mut usize) {
        if let Some(v) = self.integer(node) {
            *slot = v as usize;
        }
    }

    fn integer(&mut self, node: &KdlNode) -> Option<u64> {
        let v = self.first(node)?;
        match v.as_integer() {
            Some(i) if i >= 0 && i <= u64::MAX as i128 => Some(i as u64),
            Some(_) => {
                self.reject(node, "value must not be negative");
                None
            }
            None => {
                self.reject(node, "value must be an integer");
                None
            }
        }
    }

    fn boolean(&mut self, node: &KdlNode) -> Option<bool> {
        let v = self.first(node)?;
        match v.as_bool() {
            Some(b) => Some(b),
            None => {
                self.reject(node, "value must be #true or #false");
                None
            }
        }
    }

    fn string(&mut self, node: &KdlNode) -> Option<String> {
        let v = self.first(node)?;
        match v.as_string() {
            Some(s) if !s.is_empty() => Some(s.to_string()),
            Some(_) => {
                self.reject(node, "value must not be empty");
                None
            }
            None => {
                self.reject(node, "value must be a string");
                None
            }
        }
    }

    fn first(&mut self, node: &KdlNode) -> Option<kdl::KdlValue> {
        match node.entries().first() {
            Some(e) => Some(e.value().clone()),
            None => {
                self.reject(node, "expected a value");
                None
            }
        }
    }

    fn reject(&mut self, node: &KdlNode, message: impl Into<String>) {
        let (line, col) = locate(self.text, node.name().span().offset());
        self.errors.push(ConfigError {
            file: self.file.to_path_buf(),
            line,
            col,
            message: message.into(),
        });
    }
}

/// 1-based line and column of a byte offset.
fn locate(text: &str, offset: usize) -> (usize, usize) {
    let off = offset.min(text.len());
    let start = text[..off].rfind('\n').map_or(0, |i| i + 1);
    (text[..off].matches('\n').count() + 1, off - start + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Config, Vec<ConfigError>) {
        let mut cfg = Config::default();
        let mut errors = Vec::new();
        apply_text(&mut cfg, Path::new("oracle-eyes.kdl"), text, &mut errors);
        (cfg, errors)
    }

    #[test]
    fn defaults_are_the_spec_5_table() {
        let c = Config::default();
        assert_eq!(c.settle_ms, 600);
        assert_eq!(c.dedup_ttl_ms, 5 * 60 * 1000);
        assert_eq!(c.rate_limit_ms, 3000);
        assert_eq!(c.fast_path_min_words, 3);
        assert_eq!(c.min_display_ms, 4000);
        assert_eq!(c.ms_per_word, 150);
        assert_eq!(c.max_display_ms, 20000);
        assert_eq!(c.fail_indicator_ms, 1500);
        assert!(!c.auto);
        assert_eq!(c.model, None);
    }

    #[test]
    fn a_document_overrides_exactly_the_keys_it_names() {
        let (c, errs) = parse(
            r#"
            timing { settle_ms 250 }
            binaries { claude "/opt/claude" }
            auto #true
            "#,
        );
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(c.settle_ms, 250);
        assert_eq!(c.claude_bin, "/opt/claude");
        assert!(c.auto);
        // Untouched keys keep their defaults.
        assert_eq!(c.min_display_ms, 4000);
        assert_eq!(c.tesseract_bin, "tesseract");
        assert_eq!(c.ocr_lang, "eng");
    }

    #[test]
    fn an_unknown_key_is_reported_not_ignored() {
        let (_, errs) = parse("timing {\n  settle_ms 100\n  setle_ms 200\n}\n");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("setle_ms"), "{}", errs[0]);
        assert_eq!(errs[0].line, 3);
    }

    #[test]
    fn an_unknown_node_is_reported() {
        let (_, errs) = parse("telemetry { enable #true }\n");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("telemetry"), "{}", errs[0]);
    }

    #[test]
    fn a_wrong_type_is_reported_and_leaves_the_default() {
        let (c, errs) = parse("answer { word_cap \"lots\" }\n");
        assert_eq!(c.word_cap, Config::default().word_cap);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("integer"), "{}", errs[0]);
    }

    #[test]
    fn a_malformed_document_names_the_file_and_line() {
        let (_, errs) = parse("timing {\n  settle_ms 600\n");
        assert_eq!(errs.len(), 1);
        let rendered = errs[0].to_string();
        assert!(rendered.starts_with("oracle-eyes.kdl:"), "{rendered}");
        assert!(errs[0].line >= 1);
    }

    #[test]
    fn later_files_win_key_by_key() {
        let mut cfg = Config::default();
        let mut errs = Vec::new();
        apply_text(
            &mut cfg,
            Path::new("/etc/eclipse/oracle-eyes.kdl"),
            "timing { settle_ms 900\n min_display_ms 1000 }",
            &mut errs,
        );
        apply_text(
            &mut cfg,
            Path::new("user.kdl"),
            "timing { settle_ms 100 }",
            &mut errs,
        );
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.settle_ms, 100);
        assert_eq!(cfg.min_display_ms, 1000);
    }
}
