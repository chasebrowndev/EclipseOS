// SPDX-License-Identifier: AGPL-3.0-only
//! The custom-widget runner: `bar { widget "<name>" { … } }` (ADR 0065).
//!
//! Runs each exec widget's command out of process, argv-exec and never
//! through an implicit shell, on this service's thread rather than a draw
//! path, with a timeout and a 4 KiB line cap, and kills it on reload or
//! removal. A command has exactly the authority of the user who wrote it into
//! their own `abyss.kdl`; this is not a plugin mechanism.
//!
//! Output is untrusted text: rendered plain, never as markup, and never
//! logged, since it may carry anything the command prints.
//!
//! `Source`-kind widgets are resolved by the GUI from its own feeds
//! ([`crate::usage`], [`crate::audio`], …); the runner ignores them.
//!
//! Exec widgets run every `interval`, one run at a time, each killed after
//! min(interval, 10 s); the last non-blank stdout line is the update. Stream
//! widgets run once and every line is an update; on exit they restart with a
//! capped exponential backoff (1 s to 60 s, reset after 60 s of healthy
//! output). Children get their own process group and `PR_SET_PDEATHSIG`, so
//! the bar dying takes them with it; dropping the [`Handle`] kills and reaps
//! every one.

mod child;
mod parse;
mod worker;

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use worker::{Job, Timing, Worker};

/// One `widget` block, as read from `get_config`'s `collections.widget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetSpec {
    /// The block's name; the bar refers to it as `custom:<name>`.
    pub name: String,
    pub kind: Kind,
    /// Icon name or path.
    pub icon: Option<String>,
    pub on_click: Option<Vec<String>>,
    pub on_scroll_up: Option<Vec<String>>,
    pub on_scroll_down: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Run `argv` every `interval`; its output is one update.
    Exec { argv: Vec<String>, interval: Duration },
    /// Run `argv` once and keep it running; each line is one update.
    Stream { argv: Vec<String> },
    /// A shipped data source rendered through `format`. Resolved by the GUI.
    Source { source: String, format: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Output {
        name: String,
        out: Output,
    },
    /// The command could not run, timed out, or printed something unusable.
    /// `reason` is ours, never the command's output.
    Failed {
        name: String,
        reason: String,
    },
}

/// One update from a command: a plain-text line, or a JSON line
/// `{text, detail, tooltip, state}`. Untrusted; never markup, never logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub text: String,
    /// Lines for the widget's revealed section.
    pub detail: Vec<String>,
    pub tooltip: Option<String>,
    /// A free-form state tag the GUI may style (e.g. `warning`).
    pub state: Option<String>,
}

enum Command {
    Reconfigure(Vec<WidgetSpec>),
    Run(Vec<String>),
    Shutdown,
}

/// The GUI's end of the runner. Dropping it kills every widget command and
/// waits for them to be reaped.
pub struct Handle {
    updates: Receiver<Update>,
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    /// The next update, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// Replace the widget set. Commands for removed or changed widgets are
    /// killed.
    pub fn reconfigure(&self, specs: Vec<WidgetSpec>) {
        let _ = self.commands.send(Command::Reconfigure(specs));
    }

    /// Fire-and-forget one argv (an `on-click` or `on-scroll-*` handler).
    pub fn run(&self, argv: Vec<String>) {
        let _ = self.commands.send(Command::Run(argv));
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

/// Start the runner. The thread lives until the [`Handle`] is dropped.
pub fn spawn(specs: Vec<WidgetSpec>) -> Handle {
    spawn_with(specs, Timing::DEFAULT)
}

fn spawn_with(specs: Vec<WidgetSpec>, t: Timing) -> Handle {
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("eclipse-custom".into())
        .spawn(move || run(specs, updates_tx, commands_rx, t))
        .ok();
    Handle {
        updates,
        commands,
        thread,
    }
}

fn run(specs: Vec<WidgetSpec>, updates: Sender<Update>, commands: Receiver<Command>, t: Timing) {
    let mut workers = Vec::new();
    reconfigure(&mut workers, specs, &updates, t);
    for command in &commands {
        match command {
            Command::Reconfigure(specs) => reconfigure(&mut workers, specs, &updates, t),
            // A failed action has no widget to report against; the click
            // simply does nothing.
            Command::Run(argv) => {
                let _ = child::run_detached(&argv);
            }
            Command::Shutdown => break,
        }
    }
    stop_all(workers);
}

/// The job a spec runs, or `None` for a `Source` widget (the GUI's).
fn job(kind: &Kind) -> Option<Job> {
    match kind {
        Kind::Exec { argv, interval } => Some(Job::Exec {
            argv: argv.clone(),
            interval: *interval,
        }),
        Kind::Stream { argv } => Some(Job::Stream { argv: argv.clone() }),
        Kind::Source { .. } => None,
    }
}

/// Diffs `specs` against the running workers by name. A widget whose job is
/// unchanged keeps running (an icon or action change does not restart it);
/// changed and removed ones are killed and reaped before new ones start.
/// The first block of a duplicated name wins.
fn reconfigure(workers: &mut Vec<Worker>, specs: Vec<WidgetSpec>, updates: &Sender<Update>, t: Timing) {
    let mut kept: Vec<Worker> = Vec::new();
    let mut start: Vec<(String, Job)> = Vec::new();
    for spec in specs {
        let Some(job) = job(&spec.kind) else { continue };
        if kept.iter().any(|w| w.name == spec.name) || start.iter().any(|(n, _)| *n == spec.name) {
            continue;
        }
        match workers.iter().position(|w| w.name == spec.name && w.job == job) {
            Some(i) => kept.push(workers.swap_remove(i)),
            None => start.push((spec.name, job)),
        }
    }
    stop_all(std::mem::take(workers));
    for (name, job) in start {
        kept.push(Worker::start(name, job, updates.clone(), t));
    }
    *workers = kept;
}

/// Signals every worker, then joins them, so they die in parallel.
fn stop_all(workers: Vec<Worker>) {
    for w in &workers {
        w.signal_stop();
    }
    drop(workers);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    const FAST: Timing = Timing {
        exec_cap: Duration::from_secs(10),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_secs(5),
        healthy: Duration::from_secs(60),
        tick: Duration::from_millis(5),
    };

    fn scratch() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "eclipse-custom-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    fn spec(name: &str, kind: Kind) -> WidgetSpec {
        WidgetSpec {
            name: name.into(),
            kind,
            icon: None,
            on_click: None,
            on_scroll_up: None,
            on_scroll_down: None,
        }
    }

    fn exec(name: &str, script: &str, interval: Duration) -> WidgetSpec {
        spec(
            name,
            Kind::Exec {
                argv: sh(script),
                interval,
            },
        )
    }

    fn stream(name: &str, script: &str) -> WidgetSpec {
        spec(name, Kind::Stream { argv: sh(script) })
    }

    fn next(h: &Handle, within: Duration) -> Option<(Update, Instant)> {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if let Some(u) = h.try_recv() {
                return Some((u, Instant::now()));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }

    fn text(u: &Update) -> Option<(&str, &str)> {
        match u {
            Update::Output { name, out } => Some((name, &out.text)),
            Update::Failed { .. } => None,
        }
    }

    fn failed(u: &Update) -> Option<(&str, &str)> {
        match u {
            Update::Failed { name, reason } => Some((name, reason)),
            Update::Output { .. } => None,
        }
    }

    /// Waits for `path` to hold a pid.
    fn pid_in(path: &Path) -> u32 {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(s) = std::fs::read_to_string(path) {
                if let Ok(p) = s.trim().parse() {
                    return p;
                }
            }
            assert!(Instant::now() < end, "no pid in {}", path.display());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Whether `pid` exists at all (a zombie counts: it is not reaped).
    fn exists(pid: u32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    /// Waits for `pid` to be gone entirely: killed *and* reaped.
    fn gone(pid: u32) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if !exists(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn exec_reports_last_nonblank_line() {
        let h = spawn_with(
            vec![exec(
                "w",
                "echo first; echo; echo '  last '; echo; echo '   '",
                Duration::from_secs(60),
            )],
            FAST,
        );
        let (u, _) = next(&h, Duration::from_secs(5)).unwrap();
        assert_eq!(text(&u), Some(("w", "  last ")));
    }

    #[test]
    fn exec_json_line() {
        let h = spawn_with(
            vec![exec(
                "j",
                r#"echo '{"text":"t","detail":["d1","d2"],"tooltip":"tt","state":"warning"}'"#,
                Duration::from_secs(60),
            )],
            FAST,
        );
        let (u, _) = next(&h, Duration::from_secs(5)).unwrap();
        assert_eq!(
            u,
            Update::Output {
                name: "j".into(),
                out: Output {
                    text: "t".into(),
                    detail: vec!["d1".into(), "d2".into()],
                    tooltip: Some("tt".into()),
                    state: Some("warning".into()),
                },
            }
        );
    }

    #[test]
    fn exec_repeats_and_reports_failure() {
        let h = spawn_with(
            vec![exec("f", "echo out; exit 3", Duration::from_millis(200))],
            FAST,
        );
        for _ in 0..2 {
            let (u, _) = next(&h, Duration::from_secs(5)).unwrap();
            assert_eq!(failed(&u), Some(("f", "exited with status 3")));
        }
        let h = spawn_with(vec![exec("n", "echo x", Duration::from_secs(60))], FAST);
        drop(h);
        let h = spawn_with(
            vec![spec(
                "missing",
                Kind::Exec {
                    argv: vec!["/nonexistent/eclipse-custom-test".into()],
                    interval: Duration::from_secs(60),
                },
            )],
            FAST,
        );
        let (u, _) = next(&h, Duration::from_secs(5)).unwrap();
        assert!(failed(&u).unwrap().1.starts_with("could not start"));
    }

    #[test]
    fn exec_timeout_kills_group_and_reaps() {
        let dir = scratch();
        let script = format!(
            "echo $$ > {d}/leader; sleep 30 & echo $! > {d}/bg; echo partial; wait",
            d = dir.display()
        );
        let t = Timing {
            exec_cap: Duration::from_millis(300),
            ..FAST
        };
        let h = spawn_with(vec![exec("slow", &script, Duration::from_secs(60))], t);
        let start = Instant::now();
        let (u, at) = next(&h, Duration::from_secs(5)).unwrap();
        assert_eq!(failed(&u), Some(("slow", "timed out after 300 ms")));
        assert!(at - start >= Duration::from_millis(250));
        let leader = pid_in(&dir.join("leader"));
        let bg = pid_in(&dir.join("bg"));
        assert!(gone(leader), "leader {leader} not reaped");
        assert!(gone(bg), "background job {bg} survived");
        drop(h);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stream_lines_restart_with_backoff() {
        let h = spawn_with(vec![stream("s", "echo a; echo b; exit 2")], FAST);
        let mut restarts = Vec::new();
        let mut failed_at = None;
        while restarts.len() < 4 {
            let (u, at) = next(&h, Duration::from_secs(10)).expect("stream stalled");
            match &u {
                Update::Output { out, .. } if out.text == "a" => {
                    if let Some(f) = failed_at.take() {
                        restarts.push(at - f);
                    }
                }
                Update::Output { out, .. } => assert_eq!(out.text, "b"),
                Update::Failed { reason, .. } => {
                    assert_eq!(reason, "exited with status 2");
                    failed_at = Some(at);
                }
            }
        }
        // 50, 100, 200, 400 ms: each at least its backoff, and growing.
        for (i, gap) in restarts.iter().enumerate() {
            let want = FAST.backoff_min * (1 << i);
            assert!(
                *gap >= want - Duration::from_millis(5),
                "gap {i}: {gap:?} < {want:?}"
            );
        }
        assert!(restarts[3] > restarts[0]);
    }

    #[test]
    fn stream_backoff_resets_after_healthy_run() {
        let t = Timing {
            healthy: Duration::from_millis(100),
            ..FAST
        };
        let h = spawn_with(vec![stream("s", "echo a; sleep 0.15; exit 1")], t);
        let mut failed_at = None;
        let mut gaps = Vec::new();
        while gaps.len() < 5 {
            let (u, at) = next(&h, Duration::from_secs(10)).expect("stream stalled");
            match &u {
                Update::Output { .. } => {
                    if let Some(f) = failed_at.take() {
                        gaps.push(at - f);
                    }
                }
                Update::Failed { .. } => failed_at = Some(at),
            }
        }
        // Without the reset the fifth gap would be 800 ms.
        assert!(gaps[4] < Duration::from_millis(600), "{gaps:?}");
    }

    #[test]
    fn stream_caps_and_strips() {
        let h = spawn_with(
            vec![stream(
                "c",
                "printf 'a\\033b\\n'; head -c 10000 /dev/zero | tr '\\0' x; echo; exec sleep 30",
            )],
            FAST,
        );
        let (u, _) = next(&h, Duration::from_secs(5)).unwrap();
        assert_eq!(text(&u), Some(("c", "ab")));
        let (u, _) = next(&h, Duration::from_secs(5)).unwrap();
        let (_, t) = text(&u).unwrap();
        assert_eq!(t.len(), parse::LINE_CAP);
    }

    #[test]
    fn source_widgets_are_ignored() {
        let h = spawn_with(
            vec![spec(
                "cpu",
                Kind::Source {
                    source: "usage.cpu".into(),
                    format: "{}%".into(),
                },
            )],
            FAST,
        );
        assert_eq!(next(&h, Duration::from_millis(200)), None);
    }

    #[test]
    fn reconfigure_diffs_by_name_and_drop_reaps() {
        let dir = scratch();
        let s = |name: &str, tag: &str| {
            stream(
                name,
                &format!("echo $$ > {}/{name}.{tag}; echo up; exec sleep 30", dir.display()),
            )
        };
        let h = spawn_with(vec![s("keep", "1"), s("change", "1"), s("remove", "1")], FAST);
        let keep = pid_in(&dir.join("keep.1"));
        let change = pid_in(&dir.join("change.1"));
        let remove = pid_in(&dir.join("remove.1"));

        let mut keep_new = s("keep", "1");
        keep_new.icon = Some("icon-changes-nothing".into());
        h.reconfigure(vec![keep_new, s("change", "2"), s("add", "1")]);
        let change2 = pid_in(&dir.join("change.2"));
        let add = pid_in(&dir.join("add.1"));
        assert!(gone(change), "changed widget's old command survived");
        assert!(gone(remove), "removed widget's command survived");
        assert!(exists(keep), "unchanged widget was restarted");
        assert!(!dir.join("keep.2").exists());

        drop(h);
        for pid in [keep, change2, add] {
            // Drop joins the runner, which reaps before returning: no wait.
            assert!(!exists(pid), "{pid} left behind after drop");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn run_is_detached_and_reaped() {
        let dir = scratch();
        let h = spawn_with(Vec::new(), FAST);
        let started = Instant::now();
        h.run(sh(&format!("echo $$ > {}/click", dir.display())));
        h.run(Vec::new()); // empty argv: nothing, no panic
        let pid = pid_in(&dir.join("click"));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(gone(pid), "action {pid} left a zombie");
        drop(h);
        let _ = std::fs::remove_dir_all(dir);
    }
}
