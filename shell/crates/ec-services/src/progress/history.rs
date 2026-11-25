// SPDX-License-Identifier: AGPL-3.0-only
//! Run-time history for the progress widget's last-resort estimate.
//!
//! The store holds, per job label, the durations (whole seconds) of its last
//! few runs that ended normally as far as the service can tell. A feed job is
//! recorded only on `done`; a scanned cargo or pacman job is recorded when it
//! vanishes, and the scan cannot see its exit status, so a cancelled or failed
//! run counts too. The median absorbs the odd outlier.
//!
//! Nothing else is kept: no argv, no paths beyond what the label already shows,
//! no timestamps. The label is stored as written, so a tool must not put a
//! secret in it: labels are display text, never a password-role value. The
//! estimate for a job in flight is the median of its label's history minus what
//! has already elapsed, and is only ever labelled [`super::EtaBasis::History`].
//!
//! [`History`] is pure. Disk IO goes through [`HistoryStore`], which the
//! progress thread drives, so tests use [`MemoryStore`] and never touch
//! `$XDG_STATE_HOME`.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

/// Durations kept per key.
pub const MAX_SAMPLES: usize = 8;
/// Keys kept in total. The least recently recorded key goes first.
pub const MAX_KEYS: usize = 64;
/// A run shorter than this is not recorded: it teaches nothing.
pub const MIN_RECORD: Duration = Duration::from_secs(3);
/// Longest duration accepted, in seconds, when recording or loading.
pub const MAX_SECS: u64 = 7 * 24 * 3600;
/// Fewest samples an estimate needs.
pub const MIN_ESTIMATE_SAMPLES: usize = 2;
/// An estimate is floored here once a run has reached its median.
pub const FLOOR: Duration = Duration::from_secs(1);
/// A run this many times its median has outlived what history can say.
pub const OVERRUN_FACTOR: f64 = 1.5;
/// Longest key, matching the wire's label limit.
const MAX_KEY: usize = ec_ipc::progress::MAX_LABEL_LEN;
/// Largest history file read.
const MAX_FILE: u64 = 64 * 1024;

/// The history key of a job label: the label itself (its head and any
/// bracket suffix, so `cargo build (EclipseOS)` is per project), lowercased
/// and trimmed. `None` for a label that cannot be a key.
pub fn key(label: &str) -> Option<String> {
    let k = label.trim().to_ascii_lowercase();
    valid_key(&k).then_some(k)
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= MAX_KEY && !k.chars().any(char::is_control)
}

/// Only these scanned tools get history: their run time is a property of the
/// work, where a copy's or download's is a property of its size.
pub fn scan_eligible(label: &str) -> bool {
    let head = label.split([' ', '(']).next().unwrap_or("");
    matches!(head, "cargo" | "pacman")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    /// Least recently recorded first.
    entries: Vec<(String, Vec<u64>)>,
}

impl History {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn samples(&self, key: &str) -> &[u64] {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map_or(&[], |(_, v)| v.as_slice())
    }

    /// Record one finished run. Refused (and `false`) when it was too short
    /// or too long to mean anything.
    pub fn record(&mut self, key: &str, took: Duration) -> bool {
        let secs = took.as_secs();
        if took < MIN_RECORD || secs > MAX_SECS || !valid_key(key) {
            return false;
        }
        let mut samples = match self.entries.iter().position(|(k, _)| k == key) {
            Some(i) => self.entries.remove(i).1,
            None => Vec::new(),
        };
        samples.push(secs);
        if samples.len() > MAX_SAMPLES {
            samples.remove(0);
        }
        self.entries.push((key.to_owned(), samples));
        if self.entries.len() > MAX_KEYS {
            self.entries.remove(0);
        }
        true
    }

    /// Median of the key's runs minus `elapsed`. `None` with fewer than
    /// [`MIN_ESTIMATE_SAMPLES`] runs, or when `elapsed` is past
    /// [`OVERRUN_FACTOR`] times the median. Between the median and that
    /// bound the answer is [`FLOOR`], not zero.
    pub fn estimate(&self, key: &str, elapsed: Duration) -> Option<Duration> {
        let mut v = self.samples(key).to_vec();
        if v.len() < MIN_ESTIMATE_SAMPLES {
            return None;
        }
        v.sort_unstable();
        let n = v.len();
        let median = if n % 2 == 1 {
            v[n / 2] as f64
        } else {
            (v[n / 2 - 1] + v[n / 2]) as f64 / 2.0
        };
        if elapsed.as_secs_f64() > median * OVERRUN_FACTOR {
            return None;
        }
        let left = (median - elapsed.as_secs_f64()).max(FLOOR.as_secs_f64());
        Some(Duration::from_secs_f64(left))
    }

    pub fn to_json(&self) -> Value {
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|(k, v)| json!({"key": k, "secs": v}))
            .collect();
        json!({"v": 1, "entries": entries})
    }

    /// Parse a stored history, dropping anything malformed or out of bounds.
    /// A wholly unreadable document gives an empty history.
    pub fn from_json(text: &str) -> Self {
        let mut h = Self::default();
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return h;
        };
        if v.get("v").and_then(Value::as_u64) != Some(1) {
            return h;
        }
        let Some(entries) = v.get("entries").and_then(Value::as_array) else {
            return h;
        };
        for e in entries.iter().rev().take(MAX_KEYS).rev() {
            let (Some(k), Some(secs)) = (
                e.get("key").and_then(Value::as_str),
                e.get("secs").and_then(Value::as_array),
            ) else {
                continue;
            };
            if !valid_key(k) || k != k.to_ascii_lowercase() || h.entries.iter().any(|(x, _)| x == k) {
                continue;
            }
            let samples: Vec<u64> = secs
                .iter()
                .filter_map(Value::as_u64)
                .filter(|s| *s <= MAX_SECS)
                .collect();
            let skip = samples.len().saturating_sub(MAX_SAMPLES);
            let samples: Vec<u64> = samples.into_iter().skip(skip).collect();
            if !samples.is_empty() {
                h.entries.push((k.to_owned(), samples));
            }
        }
        h
    }
}

/// Where the history lives between runs. Driven from the progress thread
/// only, never from the GUI.
pub trait HistoryStore: Send {
    fn load(&mut self) -> History;
    fn save(&mut self, history: &History);
}

/// No persistence at all.
pub struct NoStore;

impl HistoryStore for NoStore {
    fn load(&mut self) -> History {
        History::default()
    }
    fn save(&mut self, _: &History) {}
}

/// In memory, for tests.
#[derive(Default)]
pub struct MemoryStore {
    pub initial: History,
    pub saved: Arc<Mutex<Vec<History>>>,
}

impl HistoryStore for MemoryStore {
    fn load(&mut self) -> History {
        self.initial.clone()
    }
    fn save(&mut self, history: &History) {
        if let Ok(mut s) = self.saved.lock() {
            s.push(history.clone());
        }
    }
}

/// `$XDG_STATE_HOME/eclipse/progress-history.json`, falling back to
/// `~/.local/state`. The file is 0600 in a 0700 directory and replaced
/// atomically.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// The session's store, or `None` without `$XDG_STATE_HOME` or `$HOME`.
    pub fn session() -> Option<Self> {
        Some(Self::at(default_path()?))
    }
}

pub fn default_path() -> Option<PathBuf> {
    let abs = |v: std::ffi::OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
    let base = std::env::var_os("XDG_STATE_HOME").and_then(abs).or_else(|| {
        std::env::var_os("HOME")
            .and_then(abs)
            .map(|h| h.join(".local/state"))
    })?;
    Some(base.join("eclipse/progress-history.json"))
}

impl HistoryStore for FileStore {
    fn load(&mut self) -> History {
        let Ok(meta) = fs::metadata(&self.path) else {
            return History::default();
        };
        if !meta.is_file() || meta.len() > MAX_FILE {
            return History::default();
        }
        fs::read_to_string(&self.path).map_or_else(|_| History::default(), |t| History::from_json(&t))
    }

    fn save(&mut self, history: &History) {
        // Best effort: history is a convenience, never a reason to fail.
        let _ = write_atomic(&self.path, history.to_json().to_string().as_bytes());
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("history path has no directory"))?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = dir.join(format!(".progress-history.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn keys_keep_the_bracket_suffix_and_fold_case() {
        assert_eq!(
            key("cargo build (EclipseOS)").as_deref(),
            Some("cargo build (eclipseos)")
        );
        assert_ne!(key("cargo build (A)"), key("cargo build (B)"));
        assert_eq!(key("  "), None);
        assert_eq!(key(&"x".repeat(MAX_KEY + 1)), None);
    }

    #[test]
    fn scan_history_is_for_cargo_and_pacman_only() {
        assert!(scan_eligible("cargo build (x)"));
        assert!(scan_eligible("pacman ripgrep"));
        assert!(scan_eligible("pacman"));
        assert!(!scan_eligible("cp \u{2192} usb"));
        assert!(!scan_eligible("curl x.iso"));
    }

    #[test]
    fn short_runs_are_not_recorded() {
        let mut h = History::default();
        assert!(!h.record("a", Duration::from_millis(2999)));
        assert!(h.is_empty());
        assert!(h.record("a", s(3)));
        assert!(!h.record("a", s(MAX_SECS + 1)));
    }

    #[test]
    fn keeps_the_last_eight_and_the_most_recent_keys() {
        let mut h = History::default();
        for i in 0..12 {
            h.record("a", s(10 + i));
        }
        assert_eq!(h.samples("a"), &[14, 15, 16, 17, 18, 19, 20, 21]);
        for i in 0..(MAX_KEYS + 5) {
            h.record(&format!("k{i}"), s(5));
        }
        assert!(h.samples("a").is_empty(), "oldest key evicted");
        assert_eq!(h.samples(&format!("k{}", MAX_KEYS + 4)).len(), 1);
        assert_eq!(h.entries.len(), MAX_KEYS);
    }

    #[test]
    fn estimate_is_median_minus_elapsed() {
        let mut h = History::default();
        h.record("a", s(100));
        assert_eq!(h.estimate("a", s(0)), None, "one sample is not enough");
        h.record("a", s(60));
        h.record("a", s(80));
        assert_eq!(h.estimate("a", s(30)), Some(s(50)));
        assert_eq!(h.estimate("b", s(0)), None);
    }

    #[test]
    fn estimate_floors_then_gives_up_when_overrunning() {
        let mut h = History::default();
        h.record("a", s(10));
        h.record("a", s(10));
        assert_eq!(h.estimate("a", s(12)), Some(FLOOR));
        assert_eq!(h.estimate("a", s(16)), None);
    }

    #[test]
    fn json_round_trips_and_drops_garbage() {
        let mut h = History::default();
        h.record("cargo build (x)", s(40));
        h.record("cargo build (x)", s(42));
        assert_eq!(History::from_json(&h.to_json().to_string()), h);

        assert!(History::from_json("nonsense").is_empty());
        assert!(History::from_json(r#"{"v":2,"entries":[]}"#).is_empty());
        let messy = r#"{"v":1,"entries":[
            {"key":"ok","secs":[5,"x",99999999999,7]},
            {"key":"UPPER","secs":[5]},
            {"key":"","secs":[5]},
            {"key":"ok","secs":[9]},
            {"key":"empty","secs":[]},
            {"secs":[1]}
        ]}"#;
        let h = History::from_json(messy);
        assert_eq!(h.samples("ok"), &[5, 7]);
        assert_eq!(h.entries.len(), 1);
    }

    #[test]
    fn file_store_is_private_atomic_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("ec-hist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("state/eclipse/progress-history.json");
        let mut st = FileStore::at(path.clone());
        assert!(st.load().is_empty());
        let mut h = History::default();
        h.record("a", s(9));
        st.save(&h);
        h.record("a", s(11));
        st.save(&h);
        assert_eq!(st.load(), h);
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        let leftovers = fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1, "no temp file left behind");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_file_is_ignored() {
        let dir = std::env::temp_dir().join(format!("ec-hist-big-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("h.json");
        fs::write(&path, vec![b' '; (MAX_FILE + 1) as usize]).unwrap();
        assert!(FileStore::at(path).load().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
