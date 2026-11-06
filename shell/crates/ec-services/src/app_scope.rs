// SPDX-License-Identifier: AGPL-3.0-only
//! Each human app launched from the desktop gets its own transient user scope,
//! and the apps on a short opt-in list quit when their last window closes.
//!
//! Why a scope: the bar and launcher are `ec-hyperion-bar.service`'s children
//! in its cgroup, which is `KillMode=control-group`, so restarting the bar
//! (an update, a crash) killed every app it had started, a live meeting
//! included. `app.slice` keeps a human app out of `agents.slice` (COMP-05 §6),
//! and `--scope` keeps the launched command as our child, which is what the
//! launch path's reaper waits on.
//!
//! Why the quit is here and not in abyss: killing a client's processes is not a
//! compositor operation (COMP-05 §7). The compositor only reports windows over
//! the control socket; this watcher decides, and stops the scope it made. A
//! scope is found by name (`app-<key>-<pid>-<seq>.scope`), so a scope made by
//! another process of ours (the launcher) is still found, and nothing else is
//! ever matched: no process names, no `pkill`.
//!
//! Nothing here is in the TCB. It holds no capability; a wrong call stops one
//! user app, which the human asked for by closing its window.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ec_ipc::{Client, EventKind};
use serde_json::Value;

/// The slice every human app's scope lives in. Systemd makes it implicitly for
/// the user manager; agents never use it (`agents.slice`, COMP-05 §6).
pub const SLICE: &str = "app.slice";

/// How long an app's window count must stay at 0 before its scope is stopped.
pub const GRACE: Duration = Duration::from_secs(5);

/// Apps (by [`key`]) whose scope is stopped once their last window closes. Not
/// every app: Steam, Discord and browsers live in the tray on purpose.
pub const QUIT_ON_LAST_WINDOW: &[&str] = &["zoom"];

/// Pause before reconnecting to the compositor after the socket fails.
const RETRY: Duration = Duration::from_secs(2);

static SEQ: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);

/// The name a scope is matched on: lowercase, `.desktop` stripped, and every
/// byte outside `[a-z0-9.]` as `_`. No `-` survives, so `-` can only be the
/// separator in a scope name. Applied to a desktop file id and to a window's
/// `app_id` alike, so `Zoom.desktop` and the window `zoom` agree.
pub fn key(app: &str) -> String {
    app.strip_suffix(".desktop")
        .unwrap_or(app)
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `app-<key>-<pid>-<seq>.scope`, the transient unit for one launch.
pub fn scope_name(key: &str, pid: u32, seq: u64) -> String {
    format!("app-{key}-{pid}-{seq}.scope")
}

/// True when `unit` is a scope this launch path made for `key`. The key has no
/// `-`, so `app-zoom-…` never matches `app-zoom-foo-…`.
pub fn owns(unit: &str, key: &str) -> bool {
    let Some(rest) = unit
        .strip_prefix("app-")
        .and_then(|r| r.strip_prefix(key))
        .and_then(|r| r.strip_prefix('-'))
        .and_then(|r| r.strip_suffix(".scope"))
    else {
        return false;
    };
    let mut parts = rest.split('-');
    let digits = |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    digits(parts.next()) && digits(parts.next()) && parts.next().is_none()
}

/// The argv that runs `argv` in a transient scope named `unit`. Pure, so the
/// shape is testable without systemd.
pub fn scoped_argv(unit: &str, argv: &[String]) -> Vec<String> {
    let mut out = vec![
        "systemd-run".to_owned(),
        "--user".to_owned(),
        "--scope".to_owned(),
        "--quiet".to_owned(),
        format!("--slice={SLICE}"),
        format!("--unit={unit}"),
        "--".to_owned(),
    ];
    out.extend(argv.iter().cloned());
    out
}

/// Wraps a launch argv in a scope for `app_id`. Falls back to `argv` unchanged
/// when `systemd-run` is missing or cannot make a user scope here, so a launch
/// never fails for want of one.
pub fn wrap(app_id: &str, argv: Vec<String>) -> Vec<String> {
    if argv.is_empty() || !systemd_run_usable() {
        return argv;
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let unit = scope_name(&key(app_id), std::process::id(), seq);
    scoped_argv(&unit, &argv)
}

/// Whether `systemd-run --user --scope` works in this session. Probed once per
/// process with a `true` in a scope; the probe's own scope exits at once.
fn systemd_run_usable() -> bool {
    static USABLE: OnceLock<bool> = OnceLock::new();
    *USABLE.get_or_init(|| {
        Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Window counts per [`key`] from a `get_windows` reply. Windows without an
/// `app_id` are not counted: nothing can name them on the opt-in list.
pub fn count_by_key(windows: &Value) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for w in windows.as_array().into_iter().flatten() {
        let Some(app) = w.get("app_id").and_then(Value::as_str).filter(|a| !a.is_empty()) else {
            continue;
        };
        *counts.entry(key(app)).or_insert(0) += 1;
    }
    counts
}

/// The grace-period state machine. Pure: the caller passes `now`.
///
/// An app is armed when its count goes from above 0 to 0, and disarmed as soon
/// as it is above 0 again. An app that was never seen with a window is never
/// armed, so a launch whose window has not appeared yet is left alone.
#[derive(Debug)]
pub struct Tracker {
    watch: &'static [&'static str],
    counts: BTreeMap<String, usize>,
    armed: BTreeMap<String, Instant>,
}

impl Tracker {
    pub fn new(watch: &'static [&'static str]) -> Self {
        Tracker {
            watch,
            counts: BTreeMap::new(),
            armed: BTreeMap::new(),
        }
    }

    /// Takes a fresh snapshot of window counts, taken at `now`.
    pub fn observe(&mut self, counts: BTreeMap<String, usize>, now: Instant) {
        for &app in self.watch {
            let prev = self.counts.get(app).copied().unwrap_or(0);
            let cur = counts.get(app).copied().unwrap_or(0);
            if cur > 0 {
                self.armed.remove(app);
            } else if prev > 0 {
                self.armed.insert(app.to_owned(), now);
            }
        }
        self.counts = counts;
    }

    /// The apps whose grace period has run out by `now`. They are disarmed.
    pub fn due(&mut self, now: Instant) -> Vec<String> {
        let due: Vec<String> = self
            .armed
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(**at) >= GRACE)
            .map(|(app, _)| app.clone())
            .collect();
        for app in &due {
            self.armed.remove(app);
        }
        due
    }

    /// When the next grace period runs out, if any is running.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.armed.values().map(|at| *at + GRACE).min()
    }
}

/// Starts the watcher thread once per process. Call it from a long-lived
/// process that is on the session (the bar); the launcher need not.
pub fn spawn_watcher() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("app-scopes".to_owned())
        .spawn(run);
    if let Err(e) = spawned {
        eprintln!("app-scopes: watcher not started: {e}");
    }
}

fn run() {
    let mut tracker = Tracker::new(QUIT_ON_LAST_WINDOW);
    loop {
        if let Ok(mut client) = Client::connect() {
            if client.subscribe(&[EventKind::Window]).is_ok() {
                let _ = serve(&mut client, &mut tracker);
            }
        }
        std::thread::sleep(RETRY);
    }
}

/// Follows the window events of one connection until it fails.
fn serve(client: &mut Client, tracker: &mut Tracker) -> Result<(), ec_ipc::Error> {
    let mut dirty = true;
    loop {
        if dirty {
            dirty = false;
            let windows = client.call("get_windows", Value::Null)?;
            tracker.observe(count_by_key(&windows), Instant::now());
        }
        let now = Instant::now();
        for app in tracker.due(now) {
            stop_scopes(&app);
        }
        match tracker.next_deadline() {
            // A grace period is running: sleep to its end. Events queue on the
            // socket meanwhile and are read below, before any stop is decided.
            Some(at) => std::thread::sleep(at.saturating_duration_since(now)),
            None => {
                let ev = client.wait_event()?;
                dirty |= ev.kind == EventKind::Window;
            }
        }
        while let Some(ev) = client.poll_event()? {
            dirty |= ev.kind == EventKind::Window;
        }
    }
}

/// Stops every scope this launch path made for `app`. Found by name through
/// systemd; a scope with another name is never touched.
fn stop_scopes(app: &str) {
    let Ok(out) = Command::new("systemctl")
        .args(["--user", "list-units", "--type=scope", "--plain", "--no-legend"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let units: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| owns(unit, app))
        .collect();
    if units.is_empty() {
        return;
    }
    let stopped = Command::new("systemctl")
        .args(["--user", "stop"])
        .args(&units)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if stopped.is_ok_and(|s| s.success()) {
        eprintln!(
            "app-scopes: {app} has no windows; stopped {} scope(s)",
            units.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn counts(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
        pairs.iter().map(|(k, n)| ((*k).to_owned(), *n)).collect()
    }

    const WATCH: &[&str] = &["zoom"];

    #[test]
    fn key_folds_case_and_strips_the_desktop_suffix() {
        assert_eq!(key("Zoom.desktop"), "zoom");
        assert_eq!(key("us.zoom.Zoom.desktop"), "us.zoom.zoom");
        assert_eq!(key("zoom"), "zoom");
        assert_eq!(key("my-app.desktop"), "my_app");
    }

    #[test]
    fn scope_names_are_app_key_pid_seq() {
        assert_eq!(scope_name("zoom", 42, 7), "app-zoom-42-7.scope");
    }

    #[test]
    fn owns_only_this_keys_launch_scopes() {
        assert!(owns("app-zoom-42-7.scope", "zoom"));
        assert!(!owns("app-zoom-foo-42-7.scope", "zoom"));
        assert!(!owns("app-zoomx-1-2.scope", "zoom"));
        assert!(!owns("app-zoom-42.scope", "zoom"));
        assert!(!owns("app-zoom-42-7.service", "zoom"));
        assert!(!owns("agent-zoom-42-7.scope", "zoom"));
        assert!(!owns("app-zoom--7.scope", "zoom"));
        assert!(!owns("app-zoom-42-7-1.scope", "zoom"));
        assert!(!owns("app-zoom-42-7.scope", "steam"));
    }

    #[test]
    fn scoped_argv_names_the_unit_and_keeps_argv_after_the_separator() {
        let argv: Vec<String> = ["/usr/bin/zoom", "--url=zoommtg://x"].map(String::from).to_vec();
        assert_eq!(
            scoped_argv("app-zoom-42-7.scope", &argv),
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "--slice=app.slice",
                "--unit=app-zoom-42-7.scope",
                "--",
                "/usr/bin/zoom",
                "--url=zoommtg://x",
            ]
        );
    }

    #[test]
    fn window_counts_group_by_key_and_skip_unnamed() {
        let reply = json!([
            {"handle": 1, "app_id": "Zoom"},
            {"handle": 2, "app_id": "zoom"},
            {"handle": 3, "app_id": "firefox"},
            {"handle": 4, "app_id": null},
            {"handle": 5, "app_id": ""},
        ]);
        assert_eq!(count_by_key(&reply), counts(&[("firefox", 1), ("zoom", 2)]));
        assert!(count_by_key(&Value::Null).is_empty());
    }

    #[test]
    fn last_window_closing_arms_then_fires_after_the_grace_period() {
        let t0 = Instant::now();
        let mut t = Tracker::new(WATCH);
        t.observe(counts(&[("zoom", 1)]), t0);
        assert!(t.due(t0).is_empty());
        assert_eq!(t.next_deadline(), None);

        t.observe(counts(&[]), t0);
        assert_eq!(t.next_deadline(), Some(t0 + GRACE));
        assert!(t.due(t0 + GRACE - Duration::from_millis(1)).is_empty());
        assert_eq!(t.due(t0 + GRACE), ["zoom"]);
        // Fired once; nothing left to stop.
        assert!(t.due(t0 + GRACE * 2).is_empty());
        assert_eq!(t.next_deadline(), None);
    }

    #[test]
    fn a_window_reappearing_in_the_grace_period_cancels_the_stop() {
        let t0 = Instant::now();
        let mut t = Tracker::new(WATCH);
        t.observe(counts(&[("zoom", 2)]), t0);
        t.observe(counts(&[]), t0);
        t.observe(counts(&[("zoom", 1)]), t0 + Duration::from_secs(2));
        assert_eq!(t.next_deadline(), None);
        assert!(t.due(t0 + GRACE * 4).is_empty());

        // A later close starts a fresh grace period from its own instant.
        let t1 = t0 + Duration::from_secs(10);
        t.observe(counts(&[]), t1);
        assert_eq!(t.due(t1 + GRACE), ["zoom"]);
    }

    #[test]
    fn an_app_never_seen_with_a_window_is_not_armed() {
        let t0 = Instant::now();
        let mut t = Tracker::new(WATCH);
        t.observe(counts(&[]), t0);
        t.observe(counts(&[]), t0 + GRACE * 2);
        assert_eq!(t.next_deadline(), None);
        assert!(t.due(t0 + GRACE * 10).is_empty());
    }

    #[test]
    fn apps_off_the_opt_in_list_are_never_armed() {
        let t0 = Instant::now();
        let mut t = Tracker::new(WATCH);
        t.observe(counts(&[("steam", 1)]), t0);
        t.observe(counts(&[]), t0);
        assert_eq!(t.next_deadline(), None);
        assert!(t.due(t0 + GRACE * 10).is_empty());
    }

    #[test]
    fn the_default_opt_in_list_is_zoom_only() {
        assert_eq!(QUIT_ON_LAST_WINDOW, ["zoom"]);
        assert_eq!(GRACE, Duration::from_secs(5));
    }
}
