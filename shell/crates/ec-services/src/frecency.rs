// SPDX-License-Identifier: AGPL-3.0-only
//! What the human launches, kept so the launchers can put it first.
//!
//! One small JSON file per consumer under `$XDG_STATE_HOME/eclipse/` (falling
//! back to `~/.local/state`): `launcher-usage.json` maps a desktop id to how
//! many times it was launched and when it last was, and `settings-usage.json`
//! does the same for the settings search's result paths. The key is a plain
//! `String` so any reader can use the same shape. Nothing typed is ever kept:
//! no query, no result list, only ids the human chose to run or open.
//!
//! Nothing here is in the TCB, and nothing here can fail a launch: a missing
//! or unreadable file is an empty store, and a write that fails is dropped.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// A launch loses half its weight every this many days.
const HALF_LIFE_DAYS: f64 = 14.0;

/// How one application has been used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub count: u64,
    /// Unix seconds of the latest launch.
    pub last_used: u64,
}

/// The whole store: desktop id to usage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Store {
    map: BTreeMap<String, Usage>,
}

/// Seconds since the epoch; zero if the clock is before it.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The launchers' store, under the state directory's `eclipse/`.
pub const LAUNCHER_FILE: &str = "launcher-usage.json";
/// The settings search's store, beside the launchers'.
pub const SETTINGS_FILE: &str = "settings-usage.json";

/// Where a store lives, from the two environment values that decide it.
fn path_from(file: &str, state_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = match state_home {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => home?.join(".local/state"),
    };
    Some(base.join("eclipse").join(file))
}

/// The launchers' store path, if the session has a home to put it in.
pub fn path() -> Option<PathBuf> {
    path_for(LAUNCHER_FILE)
}

/// The path of the store called `file`.
pub fn path_for(file: &str) -> Option<PathBuf> {
    path_from(
        file,
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

impl Store {
    /// The store on disk. Missing, unreadable or corrupt is empty, never an
    /// error: history is a convenience, and a launcher that refuses to open
    /// over it has its priorities backwards.
    pub fn load() -> Store {
        Store::load_file(LAUNCHER_FILE)
    }

    /// The store called `file` (`LAUNCHER_FILE`, `SETTINGS_FILE`).
    pub fn load_file(file: &str) -> Store {
        path_for(file).map(|p| Store::load_from(&p)).unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Store {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Store::default();
        };
        Store::parse(&text)
    }

    /// Entries that are not `{count, last_used}` of two unsigned integers are
    /// skipped one by one, so one bad row does not cost the rest.
    fn parse(text: &str) -> Store {
        let Ok(Value::Object(top)) = serde_json::from_str::<Value>(text) else {
            return Store::default();
        };
        let map = top
            .into_iter()
            .filter_map(|(id, v)| {
                let count = v.get("count")?.as_u64()?;
                let last_used = v.get("last_used")?.as_u64()?;
                Some((id, Usage { count, last_used }))
            })
            .collect();
        Store { map }
    }

    fn render(&self) -> String {
        let obj: serde_json::Map<String, Value> = self
            .map
            .iter()
            .map(|(id, u)| (id.clone(), json!({ "count": u.count, "last_used": u.last_used })))
            .collect();
        Value::Object(obj).to_string()
    }

    /// Write atomically: a temp file in the same directory, then a rename, so
    /// a reader never sees half a file and a crash never loses the old one.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = path
            .parent()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".launcher-usage.{}.tmp", std::process::id()));
        let written = (|| {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(self.render().as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Usage> {
        self.map.get(id)
    }

    /// Note one launch of `id` at unix time `at`.
    pub fn record_at(&mut self, id: &str, at: u64) {
        let u = self.map.entry(id.to_owned()).or_insert(Usage {
            count: 0,
            last_used: at,
        });
        u.count = u.count.saturating_add(1);
        u.last_used = u.last_used.max(at);
    }

    /// Launch count, halved for every [`HALF_LIFE_DAYS`] since the last one.
    /// An id never launched scores zero.
    pub fn score(&self, id: &str, at: u64) -> f64 {
        let Some(u) = self.map.get(id) else {
            return 0.0;
        };
        let age_days = at.saturating_sub(u.last_used) as f64 / 86_400.0;
        u.count as f64 * 0.5f64.powf(age_days / HALF_LIFE_DAYS)
    }

    /// Forget every id that `keep` does not name.
    pub fn prune(&mut self, keep: impl Fn(&str) -> bool) {
        self.map.retain(|id, _| keep(id));
    }
}

/// Remember that `id` was just launched. Re-reads the file, so two launchers
/// recording at once lose at most one tick rather than one's whole history.
/// `installed` names the ids that still exist; the rest are pruned. Called
/// before the launcher exits, and never an error to the caller.
pub fn record_launch(id: &str, installed: &BTreeSet<&str>) {
    record_use(LAUNCHER_FILE, id, |k| {
        // A `PATH` binary's id is its path: it is installed while the file is.
        if k.starts_with('/') {
            Path::new(k).exists()
        } else {
            installed.contains(k)
        }
    });
}

/// Remember one use of `id` in the store called `file`, keeping only the ids
/// `keep` names. Never an error to the caller.
pub fn record_use(file: &str, id: &str, keep: impl Fn(&str) -> bool) {
    let Some(path) = path_for(file) else {
        return;
    };
    let mut store = Store::load_from(&path);
    store.record_at(id, now());
    store.prune(|k| k == id || keep(k));
    let _ = store.save_to(&path);
}

/// Remove one store file; one that is already gone is success.
fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Forget the history in the store called `file`.
pub fn clear_file(file: &str) -> std::io::Result<()> {
    path_for(file).map_or(Ok(()), |p| remove(&p))
}

/// Remove every known store in `dir`. Both are attempted; the first error,
/// if any, is returned.
fn clear_dir(dir: &Path) -> std::io::Result<()> {
    let launcher = remove(&dir.join(LAUNCHER_FILE));
    let settings = remove(&dir.join(SETTINGS_FILE));
    launcher.and(settings)
}

/// Forget all history: the launchers' and the settings search's.
pub fn clear_all() -> std::io::Result<()> {
    path_for(LAUNCHER_FILE)
        .and_then(|p| p.parent().map(Path::to_owned))
        .map_or(Ok(()), |dir| clear_dir(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ec-frecency-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("eclipse/launcher-usage.json")
    }

    #[test]
    fn the_path_follows_xdg_state_home_then_home() {
        assert_eq!(
            path_from(LAUNCHER_FILE, Some("/s".into()), Some("/h".into())),
            Some("/s/eclipse/launcher-usage.json".into())
        );
        assert_eq!(
            path_from(LAUNCHER_FILE, Some("".into()), Some("/h".into())),
            Some("/h/.local/state/eclipse/launcher-usage.json".into())
        );
        assert_eq!(path_from(LAUNCHER_FILE, None, None), None);
        assert_eq!(
            path_from(SETTINGS_FILE, Some("/s".into()), None),
            Some("/s/eclipse/settings-usage.json".into())
        );
    }

    #[test]
    fn a_store_round_trips_through_disk() {
        let p = scratch("roundtrip");
        let mut s = Store::default();
        s.record_at("a.desktop", 100);
        s.record_at("a.desktop", 200);
        s.record_at("b.desktop", 50);
        s.save_to(&p).unwrap();
        assert_eq!(Store::load_from(&p), s);
        assert_eq!(
            s.get("a.desktop"),
            Some(&Usage {
                count: 2,
                last_used: 200
            })
        );
        // No temp file is left behind.
        let left = std::fs::read_dir(p.parent().unwrap()).unwrap().count();
        assert_eq!(left, 1);
        let _ = std::fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_missing_or_corrupt_store_is_empty() {
        assert!(Store::load_from(Path::new("/nonexistent/x/launcher-usage.json")).is_empty());
        for bad in ["", "not json", "[1,2]", "null", "{\"a\":"] {
            assert!(Store::parse(bad).is_empty(), "{bad:?}");
        }
        // One bad row costs only itself.
        let s = Store::parse(r#"{"a":{"count":2,"last_used":5},"b":"x","c":{"count":-1,"last_used":1}}"#);
        assert_eq!(
            s.get("a"),
            Some(&Usage {
                count: 2,
                last_used: 5
            })
        );
        assert!(s.get("b").is_none() && s.get("c").is_none());
    }

    #[test]
    fn a_launch_loses_half_its_weight_in_two_weeks() {
        let mut s = Store::default();
        s.record_at("a", 0);
        assert!((s.score("a", 0) - 1.0).abs() < 1e-9);
        assert!((s.score("a", 14 * DAY) - 0.5).abs() < 1e-9);
        assert!((s.score("a", 28 * DAY) - 0.25).abs() < 1e-9);
        assert_eq!(s.score("never", 0), 0.0);
    }

    #[test]
    fn recent_use_can_beat_a_larger_stale_count() {
        let mut s = Store::default();
        for _ in 0..4 {
            s.record_at("old", 0);
        }
        s.record_at("new", 42 * DAY);
        // 4 launches 42 days ago are worth 0.5; one launch today is worth 1.
        assert!(s.score("new", 42 * DAY) > s.score("old", 42 * DAY));
    }

    #[test]
    fn prune_drops_what_is_no_longer_installed() {
        let mut s = Store::default();
        s.record_at("here.desktop", 1);
        s.record_at("gone.desktop", 1);
        s.prune(|id| id == "here.desktop");
        assert!(s.get("here.desktop").is_some());
        assert!(s.get("gone.desktop").is_none());
    }

    /// `clear_all` empties both files; checked through its directory helper
    /// on a private state home so the human's own history is never touched.
    #[test]
    fn clearing_empties_the_launcher_and_the_settings_store() {
        let dir = scratch("clear-all").parent().unwrap().to_owned();
        let (l, s) = (dir.join(LAUNCHER_FILE), dir.join(SETTINGS_FILE));
        let mut store = Store::default();
        store.record_at("a", 1);
        store.save_to(&l).unwrap();
        store.save_to(&s).unwrap();
        assert!(!Store::load_from(&l).is_empty() && !Store::load_from(&s).is_empty());
        clear_dir(&dir).unwrap();
        assert!(Store::load_from(&l).is_empty() && Store::load_from(&s).is_empty());
        // Clearing what is already gone is fine.
        clear_dir(&dir).unwrap();
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
