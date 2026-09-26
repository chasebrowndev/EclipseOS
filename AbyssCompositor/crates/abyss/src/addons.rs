// SPDX-License-Identifier: AGPL-3.0-only
//! Add-on manifests and the hook set (ADR 0066; COMP-18 §3, E-01).
//!
//! An add-on is a package that ships one manifest in [`SYSTEM_DIR`]. A hook is
//! a small, generic behaviour abyss already carries that stays off unless at
//! least one installed manifest names it. The rest of the compositor asks
//! [`HookSet::is_on`] and never "is add-on X installed?".
//!
//! A hook gates what exists; it grants nothing. The gate table, the owner-uid
//! check and `policy.kdl` all still apply on top, and no manifest key can turn
//! capture on (`capture "request"` is only reported, never acted on).
//!
//! The directory is re-read on change through inotify on the compositor loop,
//! debounced like the config watcher (`config/watch.rs`). Nothing here runs on
//! another thread.

use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    time::Duration,
};

use kdl::KdlDocument;
use smithay::reexports::calloop::{
    generic::Generic,
    timer::{TimeoutAction, Timer},
    Interest, LoopHandle, Mode, PostAction,
};

use crate::state::AbyssState;

/// Where manifests live. Package-owned and root-writable (ADR 0066 "Manifest
/// trust"): a user-level process must not be able to turn a hook on, so this is
/// a constant and never an env var, a CLI flag or a config key. Only the
/// private [`Addons::load_from`] takes another directory, and only tests call it.
pub const SYSTEM_DIR: &str = "/usr/share/eclipse/addons";

/// Same window as the config watcher: absorbs a package manager writing and
/// renaming a file, short enough to feel immediate.
const DEBOUNCE: Duration = Duration::from_millis(100);

/// The abyss hooks at v1 (ADR 0066 table). A closed set: adding one is a host
/// change with its own review, never something a manifest can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    /// `annotation_*` methods; annotation binds forward on `keybind`.
    Annotations,
    /// The `annotation-select` bind action (the region selector).
    RegionSelect,
    /// The `widget` collection and its socket writes.
    TaskbarWidgets,
}

impl Hook {
    pub const ALL: [Hook; 3] = [Hook::Annotations, Hook::RegionSelect, Hook::TaskbarWidgets];

    pub fn name(self) -> &'static str {
        match self {
            Hook::Annotations => "annotations",
            Hook::RegionSelect => "region-select",
            Hook::TaskbarWidgets => "taskbar-widgets",
        }
    }

    /// The refusal a gated method answers with. Static so the gate's
    /// `Decision::Deny(&'static str)` carries it without allocating.
    pub fn off_reason(self) -> &'static str {
        match self {
            Hook::Annotations => "add-on hook `annotations` is off",
            Hook::RegionSelect => "add-on hook `region-select` is off",
            Hook::TaskbarWidgets => "add-on hook `taskbar-widgets` is off",
        }
    }

    fn from_name(s: &str) -> Option<Hook> {
        Hook::ALL.into_iter().find(|h| h.name() == s)
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Hooks other hosts own. A manifest naming one is valid (Oracle's and Fog's
/// manifests share the format), but it turns nothing on in abyss.
const FOREIGN_HOOKS: &[&str] = &["activity-lens"];

/// Which hooks are on. A bitset so the input path's check is one bool read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HookSet(u8);

impl HookSet {
    pub fn is_on(self, hook: Hook) -> bool {
        self.0 & hook.bit() != 0
    }

    pub fn insert(&mut self, hook: Hook) {
        self.0 |= hook.bit();
    }

    pub fn names(self) -> Vec<&'static str> {
        Hook::ALL
            .into_iter()
            .filter(|h| self.is_on(*h))
            .map(Hook::name)
            .collect()
    }
}

/// One installed add-on, as its manifest declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    /// Every hook the manifest names, in its order, including other hosts'.
    pub hooks: Vec<String>,
    /// `capture "request"`: shown to the owner, granted only in `policy.kdl`.
    pub capture_requested: bool,
}

/// Installed add-ons and the hooks they turn on. Owned by `AbyssState`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Addons {
    pub manifests: Vec<Manifest>,
    pub hooks: HookSet,
}

impl Addons {
    /// Read [`SYSTEM_DIR`]. A missing directory is "no add-ons", not an error.
    pub fn load() -> Addons {
        Addons::load_from(Path::new(SYSTEM_DIR))
    }

    /// Private on purpose: the directory is not a parameter anything outside
    /// this module can choose (ADR 0066 "Manifest trust").
    fn load_from(dir: &Path) -> Addons {
        let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "reading add-on manifests; running with none");
                Vec::new()
            }
        };
        files.sort();
        let mut parsed = Vec::new();
        for path in files {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                tracing::warn!(file = %path.display(), "add-on manifest name is not UTF-8; skipped");
                continue;
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "reading add-on manifest; skipped");
                    continue;
                }
            };
            let m = match parse(stem, &text) {
                Ok(m) => m,
                Err(why) => {
                    tracing::warn!(file = %path.display(), "add-on manifest skipped: {why}");
                    continue;
                }
            };
            parsed.push(m);
        }
        Addons::from_manifests(parsed)
    }

    /// Fold parsed manifests, in sorted file order, into the installed set.
    /// A duplicate `id` keeps the first. (With `id` bound to the file stem a
    /// duplicate cannot come from one directory; the rule stands anyway.)
    fn from_manifests(parsed: Vec<Manifest>) -> Addons {
        let mut out = Addons::default();
        for m in parsed {
            if out.manifests.iter().any(|o| o.id == m.id) {
                tracing::warn!(id = m.id, "duplicate add-on id; the first one wins");
                continue;
            }
            for h in m.hooks.iter().filter_map(|h| Hook::from_name(h)) {
                out.hooks.insert(h);
            }
            out.manifests.push(m);
        }
        out
    }
}

/// Parse one manifest. `stem` is the file name without `.kdl`; the `id` must
/// equal it, so a package cannot shadow another add-on's id under its own file.
fn parse(stem: &str, text: &str) -> Result<Manifest, String> {
    let doc: KdlDocument = text.parse().map_err(|e| format!("not KDL: {e}"))?;
    let (mut id, mut name, mut hooks, mut capture) = (None, None, None, false);
    let mut seen: Vec<&str> = Vec::new();
    for node in doc.nodes() {
        let key = node.name().value();
        if seen.contains(&key) {
            return Err(format!("`{key}` given twice"));
        }
        seen.push(key);
        if node.children().is_some() {
            return Err(format!("`{key}` takes no block"));
        }
        let mut args = Vec::new();
        for e in node.entries() {
            if e.name().is_some() {
                return Err(format!("`{key}` takes no properties"));
            }
            let s = e
                .value()
                .as_string()
                .ok_or_else(|| format!("`{key}` takes strings"))?;
            args.push(s.to_owned());
        }
        let one = |args: Vec<String>| -> Result<String, String> {
            match <[String; 1]>::try_from(args) {
                Ok([s]) if !s.is_empty() => Ok(s),
                _ => Err(format!("`{key}` takes exactly one non-empty string")),
            }
        };
        match key {
            "id" => id = Some(one(args)?),
            "name" => name = Some(one(args)?),
            "hooks" => {
                if args.is_empty() {
                    return Err("`hooks` names no hook".into());
                }
                for h in &args {
                    if Hook::from_name(h).is_none() && !FOREIGN_HOOKS.contains(&h.as_str()) {
                        return Err(format!("unknown hook {h:?}"));
                    }
                }
                hooks = Some(args);
            }
            "capture" => {
                if one(args)? != "request" {
                    return Err("`capture` only takes \"request\"".into());
                }
                capture = true;
            }
            other => return Err(format!("unknown key `{other}`")),
        }
    }
    let id = id.ok_or("missing `id`")?;
    let name = name.ok_or("missing `name`")?;
    if id != stem {
        return Err(format!("id {id:?} does not match the file name {stem:?}.kdl"));
    }
    Ok(Manifest {
        id,
        name,
        hooks: hooks.unwrap_or_default(),
        capture_requested: capture,
    })
}

/// The `get_config` view: `addons` and `hooks_on`.
pub fn json(a: &Addons) -> (serde_json::Value, serde_json::Value) {
    let list: Vec<serde_json::Value> = a
        .manifests
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "name": m.name,
                "hooks": m.hooks,
                "capture_requested": m.capture_requested,
            })
        })
        .collect();
    (serde_json::json!(list), serde_json::json!(a.hooks.names()))
}

/// Load the installed add-ons, make the live config agree with the hooks, and
/// watch the directory. A watch failure is logged and the compositor runs on
/// with what it loaded.
pub fn start(state: &mut AbyssState, handle: &LoopHandle<'static, AbyssState>) {
    state.addons = Addons::load();
    log_hooks(&state.addons);
    // The config was loaded before the hooks were known.
    crate::config::drop_unhooked_widgets(&mut state.config, state.addons.hooks);

    // SAFETY: `inotify_init1` takes only flags and returns a new fd or -1.
    let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if raw < 0 {
        tracing::warn!(error = %std::io::Error::last_os_error(), "inotify unavailable; add-on changes need a restart");
        return;
    }
    // SAFETY: `raw` is a fresh, owned, valid descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // The parent too, so a directory created by the first add-on install is
    // noticed; each event re-adds the directory's own watch (idempotent).
    let parent = Path::new(SYSTEM_DIR).parent().unwrap_or(Path::new("/"));
    let raw = fd.as_raw_fd();
    let watched = [add_watch(raw, parent), add_watch(raw, Path::new(SYSTEM_DIR))];
    if !watched.contains(&true) {
        tracing::debug!(dir = SYSTEM_DIR, "no add-on directory to watch");
        return;
    }
    let inserted = handle.insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, fd, state| {
        drain(fd.as_raw_fd());
        add_watch(fd.as_raw_fd(), Path::new(SYSTEM_DIR));
        schedule(state);
        Ok(PostAction::Continue)
    });
    if let Err(e) = inserted {
        tracing::warn!(%e, "inserting the add-on watcher");
    }
}

fn add_watch(fd: std::os::fd::RawFd, dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    let mask = libc::IN_CLOSE_WRITE
        | libc::IN_MOVED_TO
        | libc::IN_MOVED_FROM
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_ONLYDIR;
    // SAFETY: `fd` is a live inotify descriptor and `c` a NUL-terminated path
    // that outlives the call.
    unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) >= 0 }
}

fn drain(fd: std::os::fd::RawFd) {
    let mut buf = [0u8; (std::mem::size_of::<libc::inotify_event>() + libc::NAME_MAX as usize + 1) * 16];
    loop {
        // SAFETY: reading into a local buffer from a valid non-blocking fd.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return;
        }
    }
}

/// One pending re-read at a time; events inside the window collapse into it.
fn schedule(state: &mut AbyssState) {
    if state.addons_pending {
        return;
    }
    let armed = state
        .loop_handle
        .insert_source(Timer::from_duration(DEBOUNCE), |_, _, state| {
            state.addons_pending = false;
            apply(state, Addons::load());
            TimeoutAction::Drop
        });
    match armed {
        Ok(_) => state.addons_pending = true,
        Err(e) => tracing::warn!(%e, "arming the add-on debounce"),
    }
}

fn log_hooks(a: &Addons) {
    let ids: Vec<&str> = a.manifests.iter().map(|m| m.id.as_str()).collect();
    tracing::info!(addons = ?ids, hooks = ?a.hooks.names(), "add-on hooks");
}

/// Swap in a re-read add-on set and switch hosts off (or on) to match.
pub fn apply(state: &mut AbyssState, next: Addons) {
    if next == state.addons {
        return;
    }
    let before = state.addons.hooks;
    state.addons = next;
    log_hooks(&state.addons);
    let now = state.addons.hooks;
    let turned_off = |h: Hook| before.is_on(h) && !now.is_on(h);

    if turned_off(Hook::RegionSelect) && state.region_select.active() {
        state.region_select.cancel();
        crate::backend::damage_all(state);
    }
    if turned_off(Hook::Annotations) {
        let owners: Vec<u64> = state.annotations.iter().map(|(_, a)| a.owner).collect();
        if owners
            .into_iter()
            .map(|o| state.annotations.clear_for(o))
            .sum::<usize>()
            > 0
        {
            crate::backend::damage_all(state);
        }
    }
    if before.is_on(Hook::TaskbarWidgets) != now.is_on(Hook::TaskbarWidgets) {
        // Re-read the files so `widget` blocks come back (or go) through the
        // one apply path. Forget our own-write hashes: the files did not
        // change, but what the live config should hold did.
        state.config_written.clear();
        crate::config::watch::reload_now(state);
    } else {
        // `get_config` reports add-ons; clients refetch on `config`.
        crate::ipc::emit(state, "config", serde_json::json!({}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("abyss-addons-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (name, text) in files {
            std::fs::write(d.join(name), text).unwrap();
        }
        d
    }

    const ORACLE: &str = "id \"oracle-eyes\"\nname \"Oracle Eyes\"\nhooks \"annotations\" \"region-select\"\ncapture \"request\"\n";

    #[test]
    fn a_good_manifest_parses() {
        let m = parse("oracle-eyes", ORACLE).unwrap();
        assert_eq!(
            m,
            Manifest {
                id: "oracle-eyes".into(),
                name: "Oracle Eyes".into(),
                hooks: vec!["annotations".into(), "region-select".into()],
                capture_requested: true,
            }
        );
        let m = parse(
            "hyperion",
            "id \"hyperion\"\nname \"Hyperion\"\nhooks \"taskbar-widgets\"\n",
        )
        .unwrap();
        assert!(!m.capture_requested);
    }

    #[test]
    fn bad_manifests_are_refused() {
        for (stem, text) in [
            ("x", "id \"x\"\nname \"X\"\nexec \"/bin/sh\"\n"),
            (
                "x",
                "id \"x\"\nname \"X\"\nhooks \"annotations\" \"root-shell\"\n",
            ),
            ("x", "id \"y\"\nname \"X\"\n"),
            ("x", "name \"X\"\n"),
            ("x", "id \"x\"\n"),
            ("x", "id \"x\"\nname \"X\"\ncapture \"grant\"\n"),
            ("x", "id \"x\"\nid \"x\"\nname \"X\"\n"),
            ("x", "id \"x\"\nname \"X\"\nhooks enabled=#true\n"),
            ("x", "id \"x\" { }\nname \"X\"\n"),
            ("x", "id \"x\"\nname \"X\"\nhooks\n"),
            ("x", "id \"x\"\nname \"X\"\nhooks 1\n"),
            ("x", "{{{"),
        ] {
            assert!(parse(stem, text).is_err(), "{text:?} parsed");
        }
    }

    /// Fog's hook parses in abyss but switches nothing on here.
    #[test]
    fn a_foreign_hook_parses_and_is_ignored() {
        let d = dir(
            "foreign",
            &[(
                "fog-activity.kdl",
                "id \"fog-activity\"\nname \"Fog activity\"\nhooks \"activity-lens\"\n",
            )],
        );
        let a = Addons::load_from(&d);
        assert_eq!(a.manifests.len(), 1);
        assert_eq!(a.manifests[0].hooks, ["activity-lens"]);
        assert_eq!(a.hooks, HookSet::default());
    }

    #[test]
    fn a_bad_manifest_enables_nothing_and_the_rest_load() {
        let d = dir(
            "mixed",
            &[
                ("oracle-eyes.kdl", ORACLE),
                (
                    "evil.kdl",
                    "id \"evil\"\nname \"Evil\"\nhooks \"taskbar-widgets\" \"nope\"\n",
                ),
                (
                    "stem.kdl",
                    "id \"other\"\nname \"S\"\nhooks \"taskbar-widgets\"\n",
                ),
                (
                    "notes.txt",
                    "id \"notes\"\nname \"N\"\nhooks \"taskbar-widgets\"\n",
                ),
            ],
        );
        let a = Addons::load_from(&d);
        assert_eq!(
            a.manifests.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["oracle-eyes"]
        );
        assert!(!a.hooks.is_on(Hook::TaskbarWidgets));
        assert!(a.hooks.is_on(Hook::Annotations));
    }

    #[test]
    fn a_duplicate_id_keeps_the_first() {
        let first = parse("a", "id \"a\"\nname \"First\"\nhooks \"annotations\"\n").unwrap();
        let second = Manifest {
            name: "Second".into(),
            hooks: vec!["taskbar-widgets".into()],
            ..first.clone()
        };
        let a = Addons::from_manifests(vec![first, second]);
        assert_eq!(a.manifests.len(), 1);
        assert_eq!(a.manifests[0].name, "First");
        assert!(!a.hooks.is_on(Hook::TaskbarWidgets), "the loser's hooks leaked");
    }

    #[test]
    fn the_hook_set_is_the_union() {
        let d = dir(
            "union",
            &[
                (
                    "hyperion.kdl",
                    "id \"hyperion\"\nname \"Hyperion\"\nhooks \"taskbar-widgets\"\n",
                ),
                ("oracle-eyes.kdl", ORACLE),
            ],
        );
        let a = Addons::load_from(&d);
        for h in Hook::ALL {
            assert!(a.hooks.is_on(h), "{}", h.name());
        }
        assert_eq!(
            a.hooks.names(),
            ["annotations", "region-select", "taskbar-widgets"]
        );
        // Sorted file order.
        assert_eq!(a.manifests[0].id, "hyperion");
    }

    #[test]
    fn a_missing_directory_is_no_addons() {
        let d = std::env::temp_dir().join(format!("abyss-addons-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(Addons::load_from(&d), Addons::default());
    }

    /// ADR 0066 "Manifest trust": the directory is a package-owned constant.
    /// No env var, no config key and no public constructor argument names it.
    #[test]
    fn the_manifest_directory_is_not_user_controlled() {
        assert_eq!(SYSTEM_DIR, "/usr/share/eclipse/addons");
        let src = include_str!("addons.rs");
        assert!(
            !src.contains(concat!("env", "::var")),
            "addons.rs reads the environment"
        );
        assert!(
            !src.contains(concat!("pub fn load", "_from")),
            "the directory became a public parameter"
        );
        for key in crate::config::schema::TABLE {
            assert!(!key.path.contains("addon"), "{} is a config key", key.path);
        }
    }
}
