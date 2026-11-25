// SPDX-License-Identifier: AGPL-3.0-only
//! Session restore after an unclean end (ADR 0127).
//!
//! Nothing here can bring a dead client back. What it can do is remember
//! *which apps were where* and, with the human's yes, launch those apps again
//! through the ordinary launcher, so the rules engine places them as it would
//! any new window. Three pieces:
//!
//! * **The sentinel**, `$XDG_STATE_HOME/eclipse/session.running`, written when
//!   the session begins and removed on a clean end. One still there at the next
//!   login means the last end was unclean. It also carries a count of
//!   consecutive unclean ends, so an app that crashes the session cannot be
//!   relaunched into a loop: from the second in a row on, no offer is made.
//! * **The manifest**, `session.json` beside it (0600, atomic, debounced):
//!   per window, a desktop-entry id, a workspace, a tile slot and an output
//!   name. No titles (human-influenced text), no argv, no environment, no cwd,
//!   no grants, no handles. The writer only ever sees desktop-entry ids that
//!   exist on this machine, and [`relaunch`] runs an id only if it still does.
//! * **The relaunch**, [`relaunch`]: each unique desktop entry is launched once
//!   through [`apps::launch`], the same path the launcher uses. Grants are not
//!   persisted and nothing is replayed, so every app starts with exactly the
//!   authority a new launch would have, and an id that is missing, a binary
//!   found on `PATH` or a `Terminal=true` entry is skipped (fail closed).
//!
//! Not TCB: it holds no capability and enforces no policy. A tampered manifest
//! can at worst ask for a launch of an installed desktop entry, which the human
//! must still accept in the offer.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app_scope;
use crate::apps::{self, Entry};

/// The sentinel's file name.
pub const SENTINEL_FILE: &str = "session.running";
/// The manifest's file name.
pub const MANIFEST_FILE: &str = "session.json";
/// Where a manifest goes after two unclean ends in a row.
pub const ASIDE_FILE: &str = "session.json.aside";

/// Most windows a manifest remembers; a runaway session cannot grow it.
pub const MAX_WINDOWS: usize = 64;
/// The writer waits for this much quiet after the last window event.
pub const DEBOUNCE: Duration = Duration::from_millis(1500);
/// The writer never writes more often than this.
pub const MIN_INTERVAL: Duration = Duration::from_secs(5);

/// Where the two files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub sentinel: PathBuf,
    pub manifest: PathBuf,
    pub aside: PathBuf,
}

impl Paths {
    /// The standard state directory; `None` without a home to put it in.
    pub fn standard() -> Option<Self> {
        let sentinel = crate::frecency::path_for(SENTINEL_FILE)?;
        Some(Self::in_dir(sentinel.parent()?))
    }

    pub fn in_dir(dir: &Path) -> Self {
        Self {
            sentinel: dir.join(SENTINEL_FILE),
            manifest: dir.join(MANIFEST_FILE),
            aside: dir.join(ASIDE_FILE),
        }
    }
}

// --- The sentinel -----------------------------------------------------------

/// How this login began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// No sentinel: the last session ended cleanly (or this is the first).
    Fresh,
    /// A sentinel of this very login: a service restarted, nothing ended.
    Resumed,
    /// A sentinel of an earlier login. `consecutive` counts unclean ends in a
    /// row, this one included, so the first is `1`.
    Unclean { consecutive: u32 },
}

/// Identifies one run of the compositor and boot: the control socket's
/// device, inode and change time, and the boot id. A service that restarts
/// inside a login sees the same token; a login after a crash or reboot does
/// not. `None` when there is no compositor socket, in which case there is
/// nothing to snapshot either.
pub fn login_token(socket: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(socket).ok()?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .unwrap_or_default()
        .trim()
        .to_owned();
    Some(format!(
        "{}:{}:{}.{}:{boot}",
        m.dev(),
        m.ino(),
        m.ctime(),
        m.ctime_nsec()
    ))
}

fn sentinel_json(login: &str, unclean: u32) -> String {
    json!({"version": 1, "login": login, "unclean": unclean}).to_string()
}

/// Read what the previous run left and arm the sentinel for this one.
///
/// A sentinel that cannot be parsed counts as unclean with nothing before it:
/// the file only ever goes missing by a clean end.
pub fn begin(paths: &Paths, login: &str) -> std::io::Result<Start> {
    let start = match std::fs::read_to_string(&paths.sentinel) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Start::Fresh,
        Err(_) => Start::Unclean { consecutive: 1 },
        Ok(text) => {
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if v.get("login").and_then(Value::as_str) == Some(login) {
                Start::Resumed
            } else {
                let before = v
                    .get("unclean")
                    .and_then(Value::as_u64)
                    .map_or(0, |n| n.min(1000) as u32);
                Start::Unclean {
                    consecutive: before + 1,
                }
            }
        }
    };
    match start {
        Start::Resumed => {}
        Start::Fresh => {
            // A manifest with no unclean end behind it is stale: the writer
            // can race a clean logout. It offers nothing; drop it.
            let _ = std::fs::remove_file(&paths.manifest);
            write_private(&paths.sentinel, &sentinel_json(login, 0))?;
        }
        Start::Unclean { consecutive } => {
            write_private(&paths.sentinel, &sentinel_json(login, consecutive))?;
        }
    }
    Ok(start)
}

/// The session ended on purpose: remove the sentinel, and the manifest with
/// it (a clean end leaves nothing to offer, and nothing on disk).
pub fn end_clean(paths: &Paths) {
    let _ = std::fs::remove_file(&paths.sentinel);
    let _ = std::fs::remove_file(&paths.manifest);
}

/// Undo [`end_clean`] for a session that turned out to carry on (a refused
/// power-off), so a crash after it is still seen.
pub fn rearm(paths: &Paths, login: &str) {
    if !paths.sentinel.exists() {
        let _ = write_private(&paths.sentinel, &sentinel_json(login, 0));
    }
}

/// What to do at login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to offer.
    Nothing,
    /// Ask the human; this is what would be reopened.
    Offer(Manifest),
    /// Two unclean ends in a row: no offer, the manifest is set aside.
    SetAside,
}

/// Pure decision from how the login began and what the manifest holds.
pub fn decide(start: Start, manifest: Manifest) -> Decision {
    match start {
        Start::Fresh | Start::Resumed => Decision::Nothing,
        Start::Unclean { consecutive } if consecutive >= 2 => Decision::SetAside,
        Start::Unclean { .. } if manifest.unique_ids().is_empty() => Decision::Nothing,
        Start::Unclean { .. } => Decision::Offer(manifest),
    }
}

/// Move the manifest out of the way (kept one generation, for a human to look
/// at; never offered).
pub fn set_aside(paths: &Paths) {
    let _ = std::fs::rename(&paths.manifest, &paths.aside);
}

// --- The manifest -----------------------------------------------------------

/// One remembered window. Nothing a window said about itself is here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Desktop-entry id, as the launcher lists it (`firefox.desktop`).
    pub entry: String,
    /// 1-based workspace, as `get_windows` reports it.
    pub workspace: Option<u32>,
    /// Position among the visible windows of its workspace, left to right.
    pub slot: Option<u32>,
    /// Output connector name (`DP-1`).
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub windows: Vec<Item>,
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

fn valid_output(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_graphic())
}

impl Manifest {
    pub fn to_json(&self) -> String {
        let windows: Vec<Value> = self
            .windows
            .iter()
            .map(|w| {
                json!({
                    "entry": w.entry,
                    "workspace": w.workspace,
                    "slot": w.slot,
                    "output": w.output,
                })
            })
            .collect();
        json!({"version": 1, "windows": windows}).to_string()
    }

    /// Reads only the four known fields of each window and drops any window
    /// whose id is not a plain desktop-entry id. Anything unparseable is empty.
    pub fn from_json(text: &str) -> Self {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return Self::default();
        };
        let windows = v
            .get("windows")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|w| {
                let entry = w.get("entry")?.as_str().filter(|s| valid_id(s))?.to_owned();
                let num = |k: &str| w.get(k).and_then(Value::as_u64).map(|n| n.min(10_000) as u32);
                Some(Item {
                    entry,
                    workspace: num("workspace"),
                    slot: num("slot"),
                    output: w
                        .get("output")
                        .and_then(Value::as_str)
                        .filter(|s| valid_output(s))
                        .map(str::to_owned),
                })
            })
            .take(MAX_WINDOWS)
            .collect();
        Self { windows }
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|t| Self::from_json(&t))
            .unwrap_or_default()
    }

    /// Distinct desktop-entry ids, in first-seen order. This is how many apps
    /// the offer says it would reopen.
    pub fn unique_ids(&self) -> Vec<&str> {
        let mut seen = BTreeSet::new();
        self.windows
            .iter()
            .map(|w| w.entry.as_str())
            .filter(|id| seen.insert(*id))
            .collect()
    }

    /// Build a manifest from `get_windows` and `get_outputs` replies. A window
    /// whose `app_id` matches no installed desktop entry is left out: there is
    /// nothing to relaunch, and its title is never read.
    pub fn from_replies(windows: &Value, outputs: &Value, installed: &[Entry]) -> Self {
        let name_of = |id: u64| {
            outputs
                .as_array()
                .into_iter()
                .flatten()
                .find(|o| o.get("id").and_then(Value::as_u64) == Some(id))
                .and_then(|o| o.get("name").and_then(Value::as_str))
                .filter(|n| valid_output(n))
                .map(str::to_owned)
        };
        struct Seen {
            entry: String,
            output_id: Option<u64>,
            workspace: Option<u32>,
            at: Option<(i64, i64)>,
        }
        let mut seen: Vec<Seen> = Vec::new();
        for w in windows.as_array().into_iter().flatten() {
            let Some(app) = w.get("app_id").and_then(Value::as_str).filter(|a| !a.is_empty()) else {
                continue;
            };
            let Some(entry) = entry_for(app, installed) else {
                continue;
            };
            let rect = w.get("rect").filter(|r| r.is_object());
            let coord = |k: &str| rect.and_then(|r| r.get(k)).and_then(Value::as_i64);
            seen.push(Seen {
                entry: entry.id.clone(),
                output_id: w.get("output").and_then(Value::as_u64),
                workspace: w
                    .get("workspace")
                    .and_then(Value::as_u64)
                    .map(|n| n.min(10_000) as u32),
                at: coord("x").zip(coord("y")),
            });
            if seen.len() == MAX_WINDOWS {
                break;
            }
        }
        // Slot: rank among the visible windows of the same output and
        // workspace, left to right, then top to bottom.
        let mut windows = Vec::with_capacity(seen.len());
        for s in &seen {
            let slot = s.at.map(|at| {
                seen.iter()
                    .filter(|o| o.output_id == s.output_id && o.workspace == s.workspace)
                    .filter_map(|o| o.at)
                    .filter(|o| *o < at)
                    .count() as u32
            });
            windows.push(Item {
                entry: s.entry.clone(),
                workspace: s.workspace,
                slot,
                output: s.output_id.and_then(name_of),
            });
        }
        Self { windows }
    }

    /// Write atomically: temp file in the same directory, 0600, then rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_private(path, &self.to_json())
    }
}

/// The installed, launchable desktop entry that a window's `app_id` names.
/// Compared the way scopes are (`app_scope::key`: case folded, `.desktop`
/// stripped), so `Firefox` and `firefox.desktop` agree. `PATH` binaries and
/// terminal entries are never offered.
pub fn entry_for<'a>(app_id: &str, installed: &'a [Entry]) -> Option<&'a Entry> {
    let want = app_scope::key(app_id);
    installed
        .iter()
        .filter(|e| launchable(e))
        .find(|e| app_scope::key(&e.id) == want)
}

fn launchable(e: &Entry) -> bool {
    !apps::is_path_binary(e) && !e.terminal
}

/// Write `text` to `path` atomically, mode 0600.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The writer's clock: write only after window events have gone quiet, and
/// never faster than [`MIN_INTERVAL`]. Pure; the caller passes `now`.
#[derive(Debug, Default)]
pub struct Debouncer {
    dirty: bool,
    last_event: Option<Instant>,
    last_write: Option<Instant>,
}

impl Debouncer {
    /// Something changed (or the first snapshot is wanted).
    pub fn touch(&mut self, now: Instant) {
        self.dirty = true;
        self.last_event = Some(now);
    }

    /// Whether to snapshot now.
    pub fn due(&self, now: Instant) -> bool {
        let quiet = self
            .last_event
            .is_none_or(|t| now.saturating_duration_since(t) >= DEBOUNCE);
        let spaced = self
            .last_write
            .is_none_or(|t| now.saturating_duration_since(t) >= MIN_INTERVAL);
        self.dirty && quiet && spaced
    }

    /// A snapshot was taken at `now`.
    pub fn done(&mut self, now: Instant) {
        self.dirty = false;
        self.last_write = Some(now);
    }

    /// Whether anything is waiting to be written.
    pub fn pending(&self) -> bool {
        self.dirty
    }
}

// --- The relaunch -----------------------------------------------------------

/// What a relaunch did, by desktop-entry id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Relaunched {
    pub launched: Vec<String>,
    pub skipped: Vec<String>,
}

/// Launch each unique desktop entry of `manifest` once, through `launch`
/// (production passes [`apps::launch`]). An id that is not installed, or is a
/// `PATH` binary or a terminal entry, is skipped; a launch that errors is
/// skipped too. Nothing but the desktop entry is used: no argument, variable
/// or directory from the manifest ever reaches the process.
pub fn relaunch(
    manifest: &Manifest,
    installed: &[Entry],
    mut launch: impl FnMut(&Entry) -> std::io::Result<()>,
) -> Relaunched {
    let mut out = Relaunched::default();
    for id in manifest.unique_ids() {
        let entry = installed.iter().find(|e| e.id == id).filter(|e| launchable(e));
        match entry.map(&mut launch) {
            Some(Ok(())) => out.launched.push(id.to_owned()),
            _ => out.skipped.push(id.to_owned()),
        }
    }
    out
}

/// [`relaunch`] with the real scan and the real launcher.
pub fn relaunch_now(manifest: &Manifest) -> Relaunched {
    let installed = apps::scan(None);
    relaunch(manifest, &installed, |e| apps::launch(e, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn dir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "ec-restore-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn entry(id: &str) -> Entry {
        Entry {
            id: id.into(),
            name: id.into(),
            comment: None,
            argv: vec![format!("/usr/bin/{id}")],
            terminal: false,
            keywords: vec![],
            prefers_discrete_gpu: false,
        }
    }

    fn item(id: &str) -> Item {
        Item {
            entry: id.into(),
            workspace: Some(1),
            slot: Some(0),
            output: Some("DP-1".into()),
        }
    }

    #[test]
    fn a_clean_start_has_no_unclean_end_and_arms_the_sentinel() {
        let p = Paths::in_dir(&dir());
        assert_eq!(begin(&p, "a").unwrap(), Start::Fresh);
        assert!(p.sentinel.exists());
    }

    #[test]
    fn a_sentinel_left_behind_is_an_unclean_end() {
        let p = Paths::in_dir(&dir());
        begin(&p, "a").unwrap();
        // Power loss: nothing removed the sentinel. The next login is another one.
        assert_eq!(begin(&p, "b").unwrap(), Start::Unclean { consecutive: 1 });
    }

    #[test]
    fn a_clean_end_resets_the_count() {
        let p = Paths::in_dir(&dir());
        begin(&p, "a").unwrap();
        assert_eq!(begin(&p, "b").unwrap(), Start::Unclean { consecutive: 1 });
        end_clean(&p);
        assert_eq!(begin(&p, "c").unwrap(), Start::Fresh);
        assert_eq!(begin(&p, "d").unwrap(), Start::Unclean { consecutive: 1 });
    }

    #[test]
    fn two_unclean_ends_in_a_row_make_no_offer_and_set_the_manifest_aside() {
        let p = Paths::in_dir(&dir());
        let m = Manifest {
            windows: vec![item("firefox.desktop")],
        };
        begin(&p, "a").unwrap();
        let first = begin(&p, "b").unwrap();
        assert!(matches!(decide(first, m.clone()), Decision::Offer(_)));
        // The offer was never answered and the session died again.
        m.save(&p.manifest).unwrap();
        let second = begin(&p, "c").unwrap();
        assert_eq!(second, Start::Unclean { consecutive: 2 });
        assert_eq!(decide(second, m.clone()), Decision::SetAside);
        set_aside(&p);
        assert!(!p.manifest.exists());
        assert!(p.aside.exists());
        assert_eq!(begin(&p, "d").unwrap(), Start::Unclean { consecutive: 3 });
    }

    #[test]
    fn a_service_restart_inside_one_login_is_not_an_unclean_end() {
        let p = Paths::in_dir(&dir());
        begin(&p, "a").unwrap();
        assert_eq!(begin(&p, "a").unwrap(), Start::Resumed);
        assert_eq!(decide(Start::Resumed, Manifest::default()), Decision::Nothing);
    }

    #[test]
    fn an_unclean_end_with_nothing_to_reopen_offers_nothing() {
        assert_eq!(
            decide(Start::Unclean { consecutive: 1 }, Manifest::default()),
            Decision::Nothing
        );
    }

    #[test]
    fn a_garbled_sentinel_counts_as_unclean() {
        let p = Paths::in_dir(&dir());
        std::fs::write(&p.sentinel, "not json").unwrap();
        assert_eq!(begin(&p, "a").unwrap(), Start::Unclean { consecutive: 1 });
    }

    #[test]
    fn a_stale_manifest_is_dropped_after_a_clean_end() {
        let p = Paths::in_dir(&dir());
        Manifest {
            windows: vec![item("x.desktop")],
        }
        .save(&p.manifest)
        .unwrap();
        assert_eq!(begin(&p, "a").unwrap(), Start::Fresh);
        assert!(!p.manifest.exists());
    }

    #[test]
    fn rearm_restores_the_sentinel_only_when_missing() {
        let p = Paths::in_dir(&dir());
        begin(&p, "a").unwrap();
        end_clean(&p);
        rearm(&p, "a");
        assert_eq!(begin(&p, "b").unwrap(), Start::Unclean { consecutive: 1 });
    }

    #[test]
    fn the_manifest_holds_no_titles_argv_env_cwd_grants_or_handles() {
        let installed = [entry("firefox.desktop")];
        let windows = json!([{
            "handle": 77,
            "app_id": "firefox",
            "title": "Bank statement - Mozilla Firefox",
            "pid": 4242,
            "output": 1,
            "workspace": 2,
            "rect": {"x": 0, "y": 0, "w": 800, "h": 600},
            "trust": "secret",
            "no_agent": true,
            "argv": ["firefox", "--private-window"],
            "env": {"TOKEN": "hunter2"},
            "cwd": "/home/me/secret",
            "grants": ["camera"]
        }]);
        let outputs = json!([{"id": 1, "name": "DP-1"}]);
        let m = Manifest::from_replies(&windows, &outputs, &installed);
        assert_eq!(
            m.windows,
            vec![Item {
                entry: "firefox.desktop".into(),
                workspace: Some(2),
                slot: Some(0),
                output: Some("DP-1".into()),
            }]
        );
        let text = m.to_json();
        for leaked in [
            "Bank",
            "Mozilla",
            "4242",
            "77",
            "private-window",
            "hunter2",
            "secret",
            "camera",
            "handle",
            "title",
            "argv",
            "env",
            "cwd",
            "grant",
            "trust",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked into {text}");
        }
        // And the keys are exactly the four.
        let v: Value = serde_json::from_str(&text).unwrap();
        let keys: BTreeSet<&str> = v["windows"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, BTreeSet::from(["entry", "workspace", "slot", "output"]));
    }

    #[test]
    fn extra_fields_in_a_stored_manifest_are_ignored_on_read() {
        let m = Manifest::from_json(
            r#"{"windows":[{"entry":"a.desktop","title":"x","argv":["rm"],"workspace":3}]}"#,
        );
        assert_eq!(m.windows.len(), 1);
        assert_eq!(m.windows[0].workspace, Some(3));
        assert!(!m.to_json().contains("rm"));
    }

    #[test]
    fn ids_that_are_paths_or_odd_are_dropped_on_read() {
        let m = Manifest::from_json(
            r#"{"windows":[{"entry":"/usr/bin/evil"},{"entry":"../x.desktop"},{"entry":"a b"},{"entry":""},{"entry":"ok.desktop"}]}"#,
        );
        assert_eq!(m.unique_ids(), vec!["ok.desktop"]);
        assert!(Manifest::from_json("garbage").windows.is_empty());
    }

    #[test]
    fn slots_follow_position_within_a_workspace() {
        let installed = [entry("a.desktop"), entry("b.desktop")];
        let w = |app: &str, x: i64| json!({"app_id": app, "output": 1, "workspace": 1, "rect": {"x": x, "y": 0, "w": 10, "h": 10}});
        let windows = json!([w("b", 500), w("a", 0)]);
        let m = Manifest::from_replies(&windows, &json!([]), &installed);
        assert_eq!(m.windows[0].slot, Some(1));
        assert_eq!(m.windows[1].slot, Some(0));
        assert_eq!(m.windows[0].output, None);
    }

    #[test]
    fn a_window_with_no_installed_entry_is_not_remembered() {
        let windows = json!([{"app_id": "mystery", "title": "t"}, {"title": "no app id"}]);
        let m = Manifest::from_replies(&windows, &json!([]), &[entry("a.desktop")]);
        assert!(m.windows.is_empty());
    }

    #[test]
    fn path_binaries_and_terminal_entries_are_never_matched() {
        let mut term = entry("htop.desktop");
        term.terminal = true;
        let installed = [entry("/usr/bin/foo"), term];
        assert!(entry_for("foo", &installed).is_none());
        assert!(entry_for("htop", &installed).is_none());
    }

    #[test]
    fn the_manifest_file_is_private_and_atomic() {
        use std::os::unix::fs::PermissionsExt;
        let p = Paths::in_dir(&dir());
        let m = Manifest {
            windows: vec![item("a.desktop")],
        };
        m.save(&p.manifest).unwrap();
        let mode = std::fs::metadata(&p.manifest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(Manifest::load(&p.manifest), m);
        let leftovers: Vec<_> = std::fs::read_dir(p.manifest.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn the_writer_waits_for_quiet_and_spaces_its_writes() {
        let t0 = Instant::now();
        let mut d = Debouncer::default();
        assert!(!d.due(t0));
        d.touch(t0);
        assert!(!d.due(t0 + Duration::from_millis(100)));
        assert!(d.due(t0 + DEBOUNCE));
        // Events keep arriving: still not due.
        d.touch(t0 + Duration::from_secs(1));
        assert!(!d.due(t0 + Duration::from_secs(2)));
        let t1 = t0 + Duration::from_secs(3);
        assert!(d.due(t1));
        d.done(t1);
        assert!(!d.pending());
        d.touch(t1 + Duration::from_secs(1));
        // Quiet long enough but too soon after the last write.
        assert!(!d.due(t1 + Duration::from_secs(3)));
        assert!(d.due(t1 + MIN_INTERVAL));
    }

    #[test]
    fn relaunch_runs_each_installed_entry_once_and_nothing_else() {
        let mut term = entry("htop.desktop");
        term.terminal = true;
        let installed = [entry("a.desktop"), entry("b.desktop"), term, entry("/usr/bin/p")];
        let m = Manifest {
            windows: [
                "a.desktop",
                "a.desktop",
                "gone.desktop",
                "htop.desktop",
                "/usr/bin/p",
                "b.desktop",
            ]
            .iter()
            .map(|id| Item {
                entry: (*id).into(),
                workspace: None,
                slot: None,
                output: None,
            })
            .collect(),
        };
        let mut ran = Vec::new();
        let r = relaunch(&m, &installed, |e| {
            ran.push(e.id.clone());
            Ok(())
        });
        assert_eq!(ran, vec!["a.desktop", "b.desktop"]);
        assert_eq!(r.launched, vec!["a.desktop", "b.desktop"]);
        assert_eq!(r.skipped, vec!["gone.desktop", "htop.desktop", "/usr/bin/p"]);
    }

    #[test]
    fn a_failed_launch_is_skipped_not_retried() {
        let installed = [entry("a.desktop")];
        let m = Manifest {
            windows: vec![item("a.desktop")],
        };
        let mut calls = 0;
        let r = relaunch(&m, &installed, |_| {
            calls += 1;
            Err(std::io::Error::other("denied"))
        });
        assert_eq!(calls, 1);
        assert!(r.launched.is_empty());
        assert_eq!(r.skipped, vec!["a.desktop"]);
    }

    #[test]
    fn the_token_differs_between_two_sockets() {
        let d = dir();
        let (a, b) = (d.join("a.sock"), d.join("b.sock"));
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();
        assert_ne!(login_token(&a), login_token(&b));
        assert!(login_token(&d.join("none")).is_none());
    }
}
