// SPDX-License-Identifier: AGPL-3.0-only
//! Eclipse system updates for the taskbar's power widget: "Update & reboot"
//! and "Update & power off" (ADR 0085).
//!
//! The widget offers the item only while an Eclipse package update is pending.
//! Running it is always a full upgrade, `pacman -Syu`, of every pending package.
//! A subset upgrade is a partial upgrade, and a partial upgrade can leave a
//! shared library out of step with the packages that link against it, so the
//! count the widget shows is the total of all pending packages.
//!
//! The upgrade runs through `pkexec` on polkit action `org.eclipse.update.apply`,
//! whose helper `/usr/lib/eclipse/ec-update-apply` accepts exactly `all` and
//! execs pacman itself. Progress goes to the progress feed as job
//! `eclipse-update`. Only exit status 0 leads to the power action, through
//! logind; every other outcome leaves the machine running.
//!
//! Discovery runs `checkupdates` on start, every [`CHECK_INTERVAL`], and on
//! [`Handle::refresh`]. Without `checkupdates` (pacman-contrib) the service
//! reports [`Update::Unavailable`] and does not retry in a loop.
//!
//! Like every watcher here: one thread, an `mpsc` channel to the GUI, and
//! [`Handle::try_recv`]. Nothing here is in the TCB. It holds no capability;
//! the privileged step is pkexec, polkit and the helper.
//!
//! The process, feed and power seams are traits so the tests never run pacman
//! or touch logind.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ec_ipc::progress::{socket_path, Request as FeedRequest};

use crate::session::{Action, Session};

/// How often pending updates are rediscovered while the service runs.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// `checkupdates` syncs a temporary database, so it can take a while on a
/// slow link. Past this it is killed and the check reads as unavailable.
const CHECK_TIMEOUT: Duration = Duration::from_secs(300);
/// Feed sends are best effort and must never stall the upgrade.
const FEED_TIMEOUT: Duration = Duration::from_millis(500);
const POLL: Duration = Duration::from_millis(20);
/// The longest one upgrade may run. Generous: a full pacman run with a large
/// download fits well inside it. Past it the group is killed, so a hung pacman
/// or a stuck polkit prompt cannot hold the single-flight ticket until restart.
const APPLY_TIMEOUT: Duration = Duration::from_secs(45 * 60);

/// The progress job the upgrade reports under.
pub const JOB_ID: &str = "eclipse-update";
const JOB_LABEL: &str = "EclipseOS update";
const PKEXEC: &str = "/usr/bin/pkexec";
/// The polkit helper. Its path is the `exec.path` annotation of
/// `org.eclipse.update.apply`; keep the two in step.
pub const HELPER: &str = "/usr/lib/eclipse/ec-update-apply";

/// One package with a pending update, as `checkupdates` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPkg {
    pub name: String,
    pub old: String,
    pub new: String,
}

/// The power action that follows a successful upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    Reboot,
    PowerOff,
}

/// Something that happened to an apply request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The upgrade is running.
    Started { then: After },
    /// Refused before anything ran: no Eclipse update pending, a discovery that
    /// is not known yet, or an upgrade already in flight.
    Refused { reason: String },
    /// The upgrade exited 0 and the power action is being requested.
    Installed { then: After },
    /// The upgrade did not finish, or logind refused the power action after
    /// it did. In the first case no power action was requested.
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// The current state. `total` counts every pending package, all of which
    /// the upgrade installs. `eclipse_pending` says whether an Eclipse package
    /// is among them, and `eclipse` lists those. The widget offers the item
    /// only when `eclipse_pending` is true.
    Pending {
        total: usize,
        eclipse_pending: bool,
        eclipse: Vec<PendingPkg>,
    },
    /// `checkupdates` is missing or failed. The widget must not offer the update.
    Unavailable,
    Event(Event),
}

/// Why a process could not be run to a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The program is not installed.
    Missing,
    TimedOut,
    Io(String),
}

/// A finished process: its exit code (`None` if a signal ended it) and stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub code: Option<i32>,
    pub stdout: String,
}

/// Runs argv, never through a shell. Implemented for real by [`SystemRunner`]
/// and by a fake in the tests.
pub trait Runner: Send + Sync {
    /// Runs to completion or `timeout`, collecting stdout.
    fn capture(&self, argv: &[String], timeout: Duration) -> Result<Captured, RunError>;
    /// Runs to completion or `timeout`, calling `line` for each stdout line as
    /// it arrives. On timeout the process group is killed and `TimedOut` is
    /// returned.
    fn stream(
        &self,
        argv: &[String],
        timeout: Duration,
        line: &mut dyn FnMut(&str),
    ) -> Result<Option<i32>, RunError>;
}

/// Where progress reports go. The real one is the progress feed socket.
pub trait Feed: Send + Sync {
    fn send(&self, req: FeedRequest);
}

/// The power action, after a successful upgrade. The real one is logind.
pub trait Power: Send + Sync {
    fn perform(&self, then: After) -> Result<(), String>;
}

/// The seams a service runs with.
#[derive(Clone)]
pub struct Deps {
    pub runner: Arc<dyn Runner>,
    pub feed: Arc<dyn Feed>,
    pub power: Arc<dyn Power>,
}

/// The real process runner. Each child leads its own process group, so a
/// timeout kills everything it started, and stdin and stderr are discarded:
/// pacman's errors are not ours to log.
pub struct SystemRunner;

fn spawn_group(argv: &[String]) -> Result<Child, RunError> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| RunError::Io("empty argv".into()))?;
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => RunError::Missing,
            kind => RunError::Io(format!("{kind:?}")),
        })
}

fn kill_group(child: &Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: killpg is a plain syscall; a group that is already gone
        // gives ESRCH, which we ignore.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
}

impl Runner for SystemRunner {
    fn capture(&self, argv: &[String], timeout: Duration) -> Result<Captured, RunError> {
        let mut child = spawn_group(argv)?;
        let Some(mut out) = child.stdout.take() else {
            kill_group(&child);
            let _ = child.wait();
            return Err(RunError::Io("no stdout pipe".into()));
        };
        // Read on its own thread so a chatty child cannot wedge the poll loop.
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = out.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    kill_group(&child);
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(RunError::TimedOut);
                }
                Ok(None) => thread::sleep(POLL),
                Err(e) => {
                    kill_group(&child);
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(RunError::Io(format!("{:?}", e.kind())));
                }
            }
        };
        let stdout = reader.join().unwrap_or_default();
        Ok(Captured {
            code: status.code(),
            stdout,
        })
    }

    fn stream(
        &self,
        argv: &[String],
        timeout: Duration,
        line: &mut dyn FnMut(&str),
    ) -> Result<Option<i32>, RunError> {
        let mut child = spawn_group(argv)?;
        let Some(out) = child.stdout.take() else {
            kill_group(&child);
            let _ = child.wait();
            return Err(RunError::Io("no stdout pipe".into()));
        };
        // Lines are read on their own thread so the deadline is enforced here,
        // on the thread that owns the child. The reader ends at EOF, or when
        // the kill closes the pipe; if it outlives us it is detached.
        let (tx, rx) = mpsc::channel::<Result<Vec<u8>, io::ErrorKind>>();
        thread::spawn(move || {
            let mut reader = BufReader::new(out);
            loop {
                let mut buf = Vec::new();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(Ok(buf)).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        let _ = tx.send(Err(e.kind()));
                        break;
                    }
                }
            }
        });
        let deadline = Instant::now() + timeout;
        let mut failed = None;
        while Instant::now() < deadline {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(buf)) => line(String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r'])),
                Ok(Err(kind)) => {
                    failed = Some(kind);
                    break;
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        loop {
            if let Some(kind) = failed {
                kill_group(&child);
                let _ = child.wait();
                return Err(RunError::Io(format!("{kind:?}")));
            }
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status.code()),
                Ok(None) if Instant::now() >= deadline => {
                    kill_group(&child);
                    let _ = child.wait();
                    return Err(RunError::TimedOut);
                }
                Ok(None) => thread::sleep(POLL),
                Err(e) => {
                    kill_group(&child);
                    let _ = child.wait();
                    return Err(RunError::Io(format!("{:?}", e.kind())));
                }
            }
        }
    }
}

/// The progress feed over its unix socket. Best effort: a missing or busy
/// feed never stops an upgrade.
pub struct SocketFeed {
    path: Option<PathBuf>,
}

impl SocketFeed {
    pub fn session() -> Self {
        Self { path: socket_path() }
    }
}

impl Feed for SocketFeed {
    fn send(&self, req: FeedRequest) {
        let Some(path) = &self.path else { return };
        let Ok(line) = req.encode() else { return };
        let Ok(mut stream) = UnixStream::connect(path) else {
            return;
        };
        let _ = stream.set_read_timeout(Some(FEED_TIMEOUT));
        let _ = stream.set_write_timeout(Some(FEED_TIMEOUT));
        if stream.write_all(format!("{line}\n").as_bytes()).is_ok() {
            // The reply is not needed; reading it keeps the server's write
            // from ever seeing a closed peer mid-request.
            let mut reply = [0u8; 256];
            let _ = stream.read(&mut reply);
        }
    }
}

/// Power through logind. Called only after a successful upgrade. Not
/// interactive: a polkit challenge fails here rather than prompting the human
/// at a moment they are not looking at the bar.
pub struct LogindPower;

impl Power for LogindPower {
    fn perform(&self, then: After) -> Result<(), String> {
        let action = match then {
            After::Reboot => Action::Reboot,
            After::PowerOff => Action::PowerOff,
        };
        let session = Session::connect().map_err(|e| e.to_string())?;
        session.perform(action, false).map_err(|e| e.to_string())
    }
}

/// A package name `checkupdates` may list: `[a-z0-9@._+-]+`, no leading `-`.
/// Lines naming anything else are dropped from the list.
pub fn valid_pkg_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'@' | b'.' | b'_' | b'+' | b'-'))
}

/// Parses `checkupdates` output: `name old -> new`, one per line. Lines that
/// do not have that shape, and names that fail [`valid_pkg_name`], are dropped.
pub fn parse_checkupdates(text: &str) -> Vec<PendingPkg> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let (name, old, arrow, new) = (it.next()?, it.next()?, it.next()?, it.next()?);
            if arrow != "->" || !valid_pkg_name(name) {
                return None;
            }
            Some(PendingPkg {
                name: name.to_owned(),
                old: old.to_owned(),
                new: new.to_owned(),
            })
        })
        .collect()
}

/// Whether a package is an Eclipse package: `eclipseos` itself, anything
/// `eclipseos-*` (every package the repo builds, see `packaging/pkg/eclipseos`),
/// or anything `ec-*`.
pub fn is_eclipse(name: &str) -> bool {
    name == "eclipseos" || name.starts_with("eclipseos-") || name.starts_with("ec-")
}

/// Builds the state from the pending list.
pub fn classify(pending: Vec<PendingPkg>) -> Update {
    let total = pending.len();
    let eclipse: Vec<PendingPkg> = pending.into_iter().filter(|p| is_eclipse(&p.name)).collect();
    Update::Pending {
        total,
        eclipse_pending: !eclipse.is_empty(),
        eclipse,
    }
}

/// The helper argv for the full upgrade. Refused unless an Eclipse update is
/// pending, so no apply can run for a state the widget was never allowed to
/// offer.
pub fn plan(eclipse_pending: bool) -> Result<Vec<String>, String> {
    if !eclipse_pending {
        return Err("no Eclipse update is pending".into());
    }
    Ok(vec![PKEXEC.to_owned(), HELPER.to_owned(), "all".to_owned()])
}

/// Reads a pacman transaction line, `(3/12) upgrading foo`, as a fraction.
/// Only the transaction verbs count; the check and load phases restart
/// their own numbering and would make the bar jump back.
pub fn upgrade_fraction(line: &str) -> Option<f32> {
    let rest = line.strip_prefix('(')?;
    let (done, rest) = rest.split_once('/')?;
    let (total, rest) = rest.split_once(')')?;
    let verb = rest.split_whitespace().next()?;
    if !matches!(verb, "upgrading" | "installing" | "reinstalling" | "removing") {
        return None;
    }
    let done: u32 = done.parse().ok()?;
    let total: u32 = total.parse().ok()?;
    (total > 0 && (1..=total).contains(&done)).then(|| done as f32 / total as f32)
}

/// Share of the job's bar given to the download phase, when pacman has said
/// how much it will download. The transaction fills the rest.
const DOWNLOAD_WEIGHT: f32 = 0.5;

/// Parses a byte size as pacman prints it: `214.35 MiB`, `120 KiB`, `80 B`.
fn parse_size(num: &str, unit: &str) -> Option<u64> {
    let n: f64 = num.parse().ok()?;
    let mult = match unit {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    (n.is_finite() && n >= 0.0).then_some((n * mult) as u64)
}

/// `Total Download Size:   214.35 MiB`, in bytes.
pub fn parse_total_download(line: &str) -> Option<u64> {
    let rest = line.trim().strip_prefix("Total Download Size:")?;
    let mut it = rest.split_whitespace();
    let (num, unit) = (it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    parse_size(num, unit)
}

/// One finished or in-flight download line of pacman's progress display:
/// ` glibc-2.40-1-x86_64   9.1 MiB  15.2 MiB/s 00:01 [#####------]  45%`.
/// `size` is the file's size, `percent` how much of it has arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadLine {
    pub size: u64,
    pub percent: u8,
}

/// Reads a download progress line. pacman redraws a bar with `\r`, so only
/// the text after the last one is the current state.
pub fn parse_download_line(line: &str) -> Option<DownloadLine> {
    let line = line.rsplit('\r').next().unwrap_or(line);
    // From the right: percent, bar, time, rate unit, rate, size unit, size, name.
    let mut t = line.split_whitespace().rev();
    let percent: u8 = t.next()?.strip_suffix('%')?.parse().ok().filter(|p| *p <= 100)?;
    let bar = t.next()?;
    if !(bar.starts_with('[') && bar.ends_with(']')) {
        return None;
    }
    if !t.next()?.contains(':') {
        return None;
    }
    if !t.next()?.ends_with("/s") {
        return None;
    }
    t.next()?.parse::<f64>().ok()?;
    let unit = t.next()?;
    let num = t.next()?;
    t.next()?; // the file name
    Some(DownloadLine {
        size: parse_size(num, unit)?,
        percent,
    })
}

/// Accumulates pacman's download lines into bytes done against the announced
/// total. Without a `Total Download Size` line it has no fraction to give.
#[derive(Debug, Default)]
pub struct DownloadTracker {
    total: Option<u64>,
    completed: u64,
    current: u64,
}

impl DownloadTracker {
    /// Feeds one output line. `true` when it was a download line or the total.
    pub fn feed(&mut self, line: &str) -> bool {
        if let Some(total) = parse_total_download(line) {
            self.total = Some(total).filter(|t| *t > 0);
            return true;
        }
        let Some(d) = parse_download_line(line) else {
            return false;
        };
        if d.percent == 100 {
            self.completed = self.completed.saturating_add(d.size);
            self.current = 0;
        } else {
            self.current = d.size / 100 * u64::from(d.percent);
        }
        true
    }

    pub fn has_total(&self) -> bool {
        self.total.is_some()
    }

    /// Fraction of the announced download that has arrived.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        let done = self.completed.saturating_add(self.current);
        Some((done as f64 / total as f64).min(1.0) as f32)
    }
}

/// Runs the discovery: `checkupdates`. A failure is `Unavailable`, not an
/// empty list: a host that cannot say what is pending is not offered an update.
pub fn discover(runner: &dyn Runner) -> Update {
    match runner.capture(&["checkupdates".to_owned()], CHECK_TIMEOUT) {
        // checkupdates exits 2 when nothing is pending.
        Ok(c) if matches!(c.code, Some(0 | 2)) => classify(parse_checkupdates(&c.stdout)),
        Ok(_) | Err(_) => Update::Unavailable,
    }
}

/// Single-flight: at most one upgrade at a time. The ticket is released when
/// dropped, including when its worker thread panics or never starts.
#[derive(Debug, Default, Clone)]
pub struct Flight(Arc<AtomicBool>);

#[derive(Debug)]
pub struct Ticket(Arc<AtomicBool>);

impl Flight {
    pub fn try_begin(&self) -> Option<Ticket> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Ticket(Arc::clone(&self.0)))
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn emit(out: &Sender<Update>, event: Event) {
    let _ = out.send(Update::Event(event));
}

/// Runs the upgrade, then, only on exit status 0, the power action. Every
/// other outcome, a timeout included, ends with `Failed` and no power request.
pub fn run_apply(deps: &Deps, argv: &[String], then: After, out: &Sender<Update>, timeout: Duration) {
    emit(out, Event::Started { then });
    deps.feed.send(FeedRequest::Start {
        id: JOB_ID.to_owned(),
        label: JOB_LABEL.to_owned(),
        total: None,
    });
    let mut last = None;
    let mut download = DownloadTracker::default();
    let result = deps.runner.stream(argv, timeout, &mut |line| {
        // With a known download size the bar is download then transaction;
        // without one it is the transaction alone, as before.
        let fraction = if download.feed(line) {
            download.fraction().map(|d| d * DOWNLOAD_WEIGHT)
        } else {
            upgrade_fraction(line).map(|t| {
                if download.has_total() {
                    DOWNLOAD_WEIGHT + (1.0 - DOWNLOAD_WEIGHT) * t
                } else {
                    t
                }
            })
        };
        if let Some(fraction) = fraction {
            if last != Some(fraction) {
                last = Some(fraction);
                deps.feed.send(FeedRequest::Fraction {
                    id: JOB_ID.to_owned(),
                    fraction,
                    eta: None,
                });
            }
        }
    });
    // Only a clean exit is a finished run; anything else must not teach the
    // progress history what an upgrade "normally" takes.
    let id = JOB_ID.to_owned();
    deps.feed.send(if matches!(result, Ok(Some(0))) {
        FeedRequest::Done { id }
    } else {
        FeedRequest::Abort { id }
    });
    match result {
        Ok(Some(0)) => {
            emit(out, Event::Installed { then });
            if let Err(reason) = deps.power.perform(then) {
                emit(
                    out,
                    Event::Failed {
                        reason: format!("updated, but the power action was refused: {reason}"),
                    },
                );
            }
        }
        Ok(Some(code)) => emit(
            out,
            Event::Failed {
                reason: format!("the upgrade exited with status {code}"),
            },
        ),
        Ok(None) => emit(
            out,
            Event::Failed {
                reason: "the upgrade was killed by a signal".into(),
            },
        ),
        Err(RunError::Missing) => emit(
            out,
            Event::Failed {
                reason: "pkexec is not installed".into(),
            },
        ),
        Err(RunError::TimedOut) => emit(
            out,
            Event::Failed {
                reason: "the upgrade timed out".into(),
            },
        ),
        Err(RunError::Io(kind)) => emit(
            out,
            Event::Failed {
                reason: format!("the upgrade could not run ({kind})"),
            },
        ),
    }
}

enum Msg {
    Refresh,
    Apply { then: After },
}

/// The GUI's end of the update service.
pub struct Handle {
    updates: Receiver<Update>,
    commands: Sender<Msg>,
}

impl Handle {
    /// The next update or event, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// Rediscover pending updates now. The result arrives as an
    /// [`Update::Pending`] or [`Update::Unavailable`], sent only when it differs
    /// from the last one.
    pub fn refresh(&self) {
        let _ = self.commands.send(Msg::Refresh);
    }

    /// Start the full upgrade, then `then` on success. Refusals and outcomes
    /// come back as [`Update::Event`]; this never blocks.
    pub fn apply(&self, then: After) {
        let _ = self.commands.send(Msg::Apply { then });
    }
}

/// Starts the service with the real runner, feed and logind.
pub fn spawn() -> Handle {
    spawn_with(
        Deps {
            runner: Arc::new(SystemRunner),
            feed: Arc::new(SocketFeed::session()),
            power: Arc::new(LogindPower),
        },
        CHECK_INTERVAL,
    )
}

/// Starts the service with injected seams and a rediscovery interval.
pub fn spawn_with(deps: Deps, interval: Duration) -> Handle {
    let (updates_tx, updates_rx) = mpsc::channel();
    let (commands_tx, commands_rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("eclipse-updates".into())
        .spawn(move || serve(commands_rx, updates_tx, deps, interval));
    if let Err(e) = spawned {
        eprintln!("eclipse-updates: service not started: {e}");
    }
    Handle {
        updates: updates_rx,
        commands: commands_tx,
    }
}

struct Service {
    deps: Deps,
    out: Sender<Update>,
    flight: Flight,
    /// The last discovery result. Apply reads its `eclipse_pending`, so an
    /// apply always acts on what the widget last showed.
    current: Option<Update>,
}

fn serve(commands: Receiver<Msg>, out: Sender<Update>, deps: Deps, interval: Duration) {
    let mut service = Service {
        deps,
        out,
        flight: Flight::default(),
        current: None,
    };
    service.refresh();
    loop {
        match commands.recv_timeout(interval) {
            Ok(Msg::Refresh) | Err(RecvTimeoutError::Timeout) => service.refresh(),
            Ok(Msg::Apply { then }) => service.apply(then),
            // The handle is gone: nobody is listening.
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

impl Service {
    fn refresh(&mut self) {
        let next = discover(self.deps.runner.as_ref());
        if self.current.as_ref() != Some(&next) {
            let _ = self.out.send(next.clone());
        }
        self.current = Some(next);
    }

    fn apply(&self, then: After) {
        let eclipse_pending = matches!(
            &self.current,
            Some(Update::Pending {
                eclipse_pending: true,
                ..
            })
        );
        let argv = match plan(eclipse_pending) {
            Ok(argv) => argv,
            Err(reason) => return emit(&self.out, Event::Refused { reason }),
        };
        let Some(ticket) = self.flight.try_begin() else {
            return emit(
                &self.out,
                Event::Refused {
                    reason: "an update is already running".into(),
                },
            );
        };
        let deps = self.deps.clone();
        let out = self.out.clone();
        let spawned = thread::Builder::new()
            .name("eclipse-update-apply".into())
            .spawn(move || {
                let _ticket = ticket;
                run_apply(&deps, &argv, then, &out, APPLY_TIMEOUT);
            });
        if let Err(e) = spawned {
            eprintln!("eclipse-updates: apply not started: {e}");
            // The closure, and with it the ticket, was dropped by the failed
            // spawn, so the flight is free again.
            emit(
                &self.out,
                Event::Failed {
                    reason: "the upgrade could not start".into(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type CaptureFn = Box<dyn Fn(&[String]) -> Result<Captured, RunError> + Send + Sync>;

    /// Fake runner: `capture` answers from a closure, `stream` replays fixed
    /// lines and returns a fixed result. Every argv is recorded.
    struct FakeRunner {
        capture: CaptureFn,
        lines: Vec<String>,
        stream_result: Result<Option<i32>, RunError>,
        stream_delay: Duration,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        fn new(capture: CaptureFn) -> Self {
            Self {
                capture,
                lines: Vec::new(),
                stream_result: Ok(Some(0)),
                stream_delay: Duration::ZERO,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn streaming(mut self, lines: &[&str], result: Result<Option<i32>, RunError>) -> Self {
            self.lines = lines.iter().map(|s| (*s).to_owned()).collect();
            self.stream_result = result;
            self
        }

        fn delayed(mut self, d: Duration) -> Self {
            self.stream_delay = d;
            self
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Runner for FakeRunner {
        fn capture(&self, argv: &[String], _timeout: Duration) -> Result<Captured, RunError> {
            self.calls.lock().unwrap().push(argv.to_vec());
            (self.capture)(argv)
        }

        fn stream(
            &self,
            argv: &[String],
            timeout: Duration,
            line: &mut dyn FnMut(&str),
        ) -> Result<Option<i32>, RunError> {
            self.calls.lock().unwrap().push(argv.to_vec());
            // Like the real runner: a process that outlives the timeout is
            // killed at it and reported as timed out.
            if self.stream_delay > timeout {
                thread::sleep(timeout);
                return Err(RunError::TimedOut);
            }
            if !self.stream_delay.is_zero() {
                thread::sleep(self.stream_delay);
            }
            for l in &self.lines {
                line(l);
            }
            self.stream_result.clone()
        }
    }

    #[derive(Default)]
    struct RecordFeed(Mutex<Vec<FeedRequest>>);

    impl Feed for RecordFeed {
        fn send(&self, req: FeedRequest) {
            self.0.lock().unwrap().push(req);
        }
    }

    struct FakePower {
        reply: Result<(), String>,
        calls: Mutex<Vec<After>>,
    }

    impl FakePower {
        fn ok() -> Self {
            Self {
                reply: Ok(()),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn refusing() -> Self {
            Self {
                reply: Err("interactive authentication required".into()),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<After> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Power for FakePower {
        fn perform(&self, then: After) -> Result<(), String> {
            self.calls.lock().unwrap().push(then);
            self.reply.clone()
        }
    }

    fn pkg(name: &str) -> PendingPkg {
        PendingPkg {
            name: name.into(),
            old: "1-1".into(),
            new: "2-1".into(),
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    fn captured(code: Option<i32>, stdout: &str) -> Result<Captured, RunError> {
        Ok(Captured {
            code,
            stdout: stdout.into(),
        })
    }

    const CHECK_OUT: &str = "eclipseos-abyss 0.1.1-1 -> 0.1.1-2\nglibc 2.40-1 -> 2.41-1\n";

    /// A runner whose checkupdates shows CHECK_OUT: two pending, one Eclipse.
    fn eclipse_runner() -> FakeRunner {
        FakeRunner::new(Box::new(|argv: &[String]| match argv[0].as_str() {
            "checkupdates" => captured(Some(0), CHECK_OUT),
            _ => Err(RunError::Missing),
        }))
    }

    fn deps(runner: FakeRunner, feed: Arc<RecordFeed>, power: Arc<FakePower>) -> (Deps, Arc<FakeRunner>) {
        let runner = Arc::new(runner);
        let d = Deps {
            runner: runner.clone(),
            feed,
            power,
        };
        (d, runner)
    }

    /// Waits for the handle to deliver an update matching `want`.
    fn wait_for(handle: &Handle, want: impl Fn(&Update) -> bool) -> Update {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(u) = handle.try_recv() {
                if want(&u) {
                    return u;
                }
            }
            assert!(Instant::now() < deadline, "timed out waiting for an update");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn checkupdates_lines_parse_and_bad_names_drop() {
        let text = "eclipseos-abyss 0.1.1-1 -> 0.1.1-2\n\
                    linux 6.1-1 -> 6.2-1 [ignored]\n\
                    Evil;rm 1 -> 2\n\
                    -rf 1 -> 2\n\
                    not an update line\n\
                    \n\
                    ec-ctl 1 -> 2\n";
        let got: Vec<String> = parse_checkupdates(text).into_iter().map(|p| p.name).collect();
        assert_eq!(got, ["eclipseos-abyss", "linux", "ec-ctl"]);
        let first = &parse_checkupdates(text)[0];
        assert_eq!((first.old.as_str(), first.new.as_str()), ("0.1.1-1", "0.1.1-2"));
    }

    #[test]
    fn eclipse_matcher_covers_the_repo_names() {
        for name in [
            "eclipseos",
            "eclipseos-abyss",
            "eclipseos-hyperion",
            "ec-ctl",
            "ec-claude-agent",
        ] {
            assert!(is_eclipse(name), "{name}");
        }
        for name in [
            "eclipseosx",
            "linux",
            "glibc",
            "ec2-api-tools",
            "xec-foo",
            "eclipse",
        ] {
            assert!(!is_eclipse(name), "{name}");
        }
    }

    #[test]
    fn classify_counts_every_pending_package_and_flags_eclipse() {
        let pending = vec![
            pkg("eclipseos-abyss"),
            pkg("glibc"),
            pkg("linux"),
            pkg("eclipseos-hyperion"),
            pkg("readline"),
        ];
        let Update::Pending {
            total,
            eclipse_pending,
            eclipse,
        } = classify(pending)
        else {
            panic!("expected Pending");
        };
        assert_eq!(total, 5);
        assert!(eclipse_pending);
        let names: Vec<_> = eclipse.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["eclipseos-abyss", "eclipseos-hyperion"]);

        assert_eq!(
            classify(vec![pkg("glibc")]),
            Update::Pending {
                total: 1,
                eclipse_pending: false,
                eclipse: vec![],
            }
        );
    }

    #[test]
    fn plan_is_always_the_full_upgrade() {
        assert_eq!(plan(true).unwrap(), argv(&[PKEXEC, HELPER, "all"]));
    }

    #[test]
    fn plan_refuses_without_an_eclipse_update() {
        assert!(plan(false).is_err());
    }

    #[test]
    fn progress_reads_transaction_lines_only() {
        assert_eq!(upgrade_fraction("(2/5) upgrading eclipseos-abyss"), Some(0.4));
        assert_eq!(upgrade_fraction("(4/4) installing glibc"), Some(1.0));
        assert_eq!(upgrade_fraction("(1/3) checking keys in keyring"), None);
        assert_eq!(upgrade_fraction("(1/3) loading package files"), None);
        assert_eq!(upgrade_fraction("(0/3) upgrading x"), None);
        assert_eq!(upgrade_fraction("(4/3) upgrading x"), None);
        assert_eq!(upgrade_fraction("upgrading x"), None);
        assert_eq!(upgrade_fraction(":: Synchronizing package databases"), None);
    }

    #[test]
    fn total_download_size_line_parses() {
        assert_eq!(
            parse_total_download("Total Download Size:   214.35 MiB"),
            Some((214.35 * 1024.0 * 1024.0) as u64)
        );
        assert_eq!(parse_total_download("Total Download Size:    0.00 MiB"), Some(0));
        assert_eq!(parse_total_download("Total Download Size:  12 KiB"), Some(12288));
        assert_eq!(parse_total_download("Total Installed Size:  900.00 MiB"), None);
        assert_eq!(parse_total_download("Total Download Size:   12 parsecs"), None);
        assert_eq!(parse_total_download("Total Download Size:   1 MiB extra"), None);
    }

    #[test]
    fn download_lines_parse_from_real_pacman_output() {
        let l =
            " glibc-2.40+r16+gaa533d58b1-1-x86_64   9.1 MiB  15.2 MiB/s 00:01 [######################]  45%";
        let d = parse_download_line(l).expect("parses");
        assert_eq!(d.percent, 45);
        assert_eq!(d.size, (9.1 * 1024.0 * 1024.0) as u64);
        let l = " core.db                          120.5 KiB  1203 KiB/s 00:00 [######################] 100%";
        assert_eq!(
            parse_download_line(l),
            Some(DownloadLine {
                size: (120.5 * 1024.0) as u64,
                percent: 100
            })
        );
        // A redrawn bar arrives as one line of carriage-return updates.
        let l = " foo-1-x86_64  2.0 MiB  1.0 MiB/s 00:02 [###---]  10%\r foo-1-x86_64  2.0 MiB  1.0 MiB/s 00:01 [######] 100%";
        assert_eq!(parse_download_line(l).unwrap().percent, 100);
    }

    #[test]
    fn non_download_lines_are_not_download_lines() {
        for l in [
            "",
            ":: Retrieving packages...",
            "(2/5) upgrading eclipseos-abyss",
            "Total Download Size:   214.35 MiB",
            " name 1 MiB 1 MiB/s 00:01 ###  50%",
            " name 1 MiB 1 MiB/s 00:01 [##]  101%",
            " name 1 MiB 1 MiB 00:01 [##]  50%",
            " name 1 parsec 1 MiB/s 00:01 [##]  50%",
            "[##]  50%",
            "50%",
        ] {
            assert_eq!(parse_download_line(l), None, "{l:?}");
        }
    }

    #[test]
    fn download_tracker_sums_files_against_the_announced_total() {
        let mut d = DownloadTracker::default();
        assert!(!d.feed(":: Retrieving packages..."));
        assert_eq!(d.fraction(), None, "no total, no fraction");
        assert!(d.feed("Total Download Size:   10.00 MiB"));
        assert_eq!(d.fraction(), Some(0.0));
        assert!(d.feed(" a-1-x86_64   4.0 MiB  1.0 MiB/s 00:04 [######] 100%"));
        assert_eq!(d.fraction(), Some(0.4));
        assert!(d.feed(" b-1-x86_64   6.0 MiB  1.0 MiB/s 00:03 [###---]  50%"));
        assert!((d.fraction().unwrap() - 0.7).abs() < 0.01);
        assert!(d.feed(" b-1-x86_64   6.0 MiB  1.0 MiB/s 00:00 [######] 100%"));
        assert_eq!(d.fraction(), Some(1.0));
        assert!(d.has_total());
    }

    #[test]
    fn upgrade_bar_is_download_then_transaction_when_a_total_is_known() {
        let feed = Arc::new(RecordFeed::default());
        let runner = FakeRunner::new(Box::new(|_| captured(Some(0), ""))).streaming(
            &[
                "Total Download Size:   10.00 MiB",
                " a-1-x86_64   10.0 MiB  1.0 MiB/s 00:04 [######] 100%",
                "(1/2) upgrading a",
                "(2/2) upgrading b",
            ],
            Ok(Some(0)),
        );
        let power = Arc::new(FakePower::ok());
        let (d, _) = deps(runner, feed.clone(), power);
        let (tx, _rx) = mpsc::channel();
        run_apply(&d, &argv(&["x"]), After::Reboot, &tx, Duration::from_secs(5));
        let fractions: Vec<f32> = feed
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| match r {
                FeedRequest::Fraction { fraction, .. } => Some(*fraction),
                _ => None,
            })
            .collect();
        assert_eq!(fractions, vec![0.0, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn flight_admits_one_upgrade_at_a_time() {
        let flight = Flight::default();
        let first = flight.try_begin().expect("first ticket");
        assert!(flight.try_begin().is_none());
        drop(first);
        assert!(flight.try_begin().is_some());
    }

    #[test]
    fn unavailable_checkupdates_is_reported_once_without_retry_loop() {
        let runner = FakeRunner::new(Box::new(|_| Err(RunError::Missing)));
        assert_eq!(discover(&runner), Update::Unavailable);
        assert_eq!(runner.calls().len(), 1, "one probe, no loop");
    }

    #[test]
    fn checkupdates_failure_is_unavailable_and_exit_two_is_nothing_pending() {
        let failing = FakeRunner::new(Box::new(|_| captured(Some(1), "")));
        assert_eq!(discover(&failing), Update::Unavailable);
        let none = FakeRunner::new(Box::new(|_| captured(Some(2), "")));
        assert_eq!(
            discover(&none),
            Update::Pending {
                total: 0,
                eclipse_pending: false,
                eclipse: vec![],
            }
        );
    }

    #[test]
    fn discovery_reports_the_total_and_the_eclipse_subset() {
        let Update::Pending {
            total,
            eclipse_pending,
            eclipse,
        } = discover(&eclipse_runner())
        else {
            panic!("expected Pending");
        };
        assert_eq!(total, 2);
        assert!(eclipse_pending);
        let names: Vec<_> = eclipse.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["eclipseos-abyss"]);
    }

    #[test]
    fn success_runs_the_upgrade_then_powers_off() {
        let feed = Arc::new(RecordFeed::default());
        let power = Arc::new(FakePower::ok());
        let runner = FakeRunner::new(Box::new(|_| Err(RunError::Missing))).streaming(
            &["(1/2) upgrading eclipseos-abyss", "(2/2) upgrading glibc"],
            Ok(Some(0)),
        );
        let (d, runner) = deps(runner, feed.clone(), power.clone());
        let (tx, rx) = mpsc::channel();
        let args = argv(&[PKEXEC, HELPER, "all"]);
        run_apply(&d, &args, After::PowerOff, &tx, APPLY_TIMEOUT);
        drop(tx);

        assert_eq!(runner.calls(), vec![args]);
        assert_eq!(power.calls(), vec![After::PowerOff]);
        let events: Vec<Update> = rx.try_iter().collect();
        assert_eq!(
            events,
            vec![
                Update::Event(Event::Started {
                    then: After::PowerOff
                }),
                Update::Event(Event::Installed {
                    then: After::PowerOff
                }),
            ]
        );
        let reqs = feed.0.lock().unwrap().clone();
        assert_eq!(
            reqs,
            vec![
                FeedRequest::Start {
                    id: JOB_ID.into(),
                    label: JOB_LABEL.into(),
                    total: None
                },
                FeedRequest::Fraction {
                    id: JOB_ID.into(),
                    fraction: 0.5,
                    eta: None
                },
                FeedRequest::Fraction {
                    id: JOB_ID.into(),
                    fraction: 1.0,
                    eta: None
                },
                FeedRequest::Done { id: JOB_ID.into() },
            ]
        );
    }

    /// The owner's rule: a failed upgrade never reaches the power action.
    #[test]
    fn failed_upgrade_never_powers_the_machine() {
        for result in [
            Ok(Some(1)),
            Ok(None),
            Err(RunError::Missing),
            Err(RunError::TimedOut),
            Err(RunError::Io("PermissionDenied".into())),
        ] {
            let feed = Arc::new(RecordFeed::default());
            let power = Arc::new(FakePower::ok());
            let runner = FakeRunner::new(Box::new(|_| Err(RunError::Missing))).streaming(&[], result.clone());
            let (d, _) = deps(runner, feed, power.clone());
            let (tx, rx) = mpsc::channel();
            run_apply(
                &d,
                &argv(&[PKEXEC, HELPER, "all"]),
                After::Reboot,
                &tx,
                APPLY_TIMEOUT,
            );
            drop(tx);
            assert!(power.calls().is_empty(), "power ran after {result:?}");
            let last = rx.try_iter().last().expect("an event");
            assert!(
                matches!(last, Update::Event(Event::Failed { .. })),
                "{result:?} ended with {last:?}"
            );
        }
    }

    #[test]
    fn refused_power_after_a_good_upgrade_is_reported_as_failed() {
        let feed = Arc::new(RecordFeed::default());
        let power = Arc::new(FakePower::refusing());
        let runner = FakeRunner::new(Box::new(|_| Err(RunError::Missing))).streaming(&[], Ok(Some(0)));
        let (d, _) = deps(runner, feed, power.clone());
        let (tx, rx) = mpsc::channel();
        run_apply(
            &d,
            &argv(&[PKEXEC, HELPER, "all"]),
            After::Reboot,
            &tx,
            APPLY_TIMEOUT,
        );
        drop(tx);
        assert_eq!(power.calls(), vec![After::Reboot]);
        let events: Vec<Update> = rx.try_iter().collect();
        assert!(matches!(events.last(), Some(Update::Event(Event::Failed { .. }))));
        assert!(events.contains(&Update::Event(Event::Installed { then: After::Reboot })));
    }

    #[test]
    fn a_hung_upgrade_fails_at_the_timeout_and_never_powers() {
        let feed = Arc::new(RecordFeed::default());
        let power = Arc::new(FakePower::ok());
        let runner = FakeRunner::new(Box::new(|_| Err(RunError::Missing)))
            .streaming(&[], Ok(Some(0)))
            .delayed(Duration::from_secs(3600));
        let (d, _) = deps(runner, feed.clone(), power.clone());
        let (tx, rx) = mpsc::channel();
        run_apply(
            &d,
            &argv(&[PKEXEC, HELPER, "all"]),
            After::PowerOff,
            &tx,
            Duration::from_millis(50),
        );
        drop(tx);
        assert!(power.calls().is_empty());
        let events: Vec<Update> = rx.try_iter().collect();
        assert_eq!(
            events.last(),
            Some(&Update::Event(Event::Failed {
                reason: "the upgrade timed out".into()
            }))
        );
        assert_eq!(
            feed.0.lock().unwrap().last(),
            Some(&FeedRequest::Abort { id: JOB_ID.into() }),
            "the widget job ends with the upgrade"
        );
    }

    #[test]
    fn the_real_runner_kills_a_child_at_its_timeout() {
        let started = Instant::now();
        let r = SystemRunner.stream(&argv(&["sleep", "30"]), Duration::from_millis(100), &mut |_| {});
        assert!(matches!(r, Err(RunError::TimedOut)), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_real_runner_passes_lines_and_status_within_the_timeout() {
        let mut lines = Vec::new();
        let r = SystemRunner.stream(
            &argv(&["sh", "-c", "printf 'a\\r\\nb\\n'; exit 3"]),
            Duration::from_secs(10),
            &mut |l| lines.push(l.to_owned()),
        );
        assert!(matches!(r, Ok(Some(3))), "{r:?}");
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn apply_without_a_pending_eclipse_update_is_refused() {
        let runner = FakeRunner::new(Box::new(|argv: &[String]| match argv[0].as_str() {
            "checkupdates" => captured(Some(2), ""),
            _ => Err(RunError::Missing),
        }));
        let feed = Arc::new(RecordFeed::default());
        let power = Arc::new(FakePower::ok());
        let (d, runner) = deps(runner, feed, power.clone());
        let handle = spawn_with(d, Duration::from_secs(3600));
        handle.apply(After::PowerOff);
        let ev = wait_for(&handle, |u| matches!(u, Update::Event(_)));
        assert!(matches!(ev, Update::Event(Event::Refused { .. })));
        assert!(power.calls().is_empty());
        assert!(runner.calls().iter().all(|c| c[0] != PKEXEC));
    }

    #[test]
    fn apply_with_only_non_eclipse_updates_pending_is_refused() {
        let runner = FakeRunner::new(Box::new(|argv: &[String]| match argv[0].as_str() {
            "checkupdates" => captured(Some(0), "glibc 2.40-1 -> 2.41-1\n"),
            _ => Err(RunError::Missing),
        }));
        let feed = Arc::new(RecordFeed::default());
        let power = Arc::new(FakePower::ok());
        let (d, runner) = deps(runner, feed, power.clone());
        let handle = spawn_with(d, Duration::from_secs(3600));
        wait_for(&handle, |u| matches!(u, Update::Pending { .. }));
        handle.apply(After::Reboot);
        let ev = wait_for(&handle, |u| matches!(u, Update::Event(_)));
        assert!(matches!(ev, Update::Event(Event::Refused { .. })));
        assert!(power.calls().is_empty());
        assert!(runner.calls().iter().all(|c| c[0] != PKEXEC));
    }

    #[test]
    fn service_applies_then_powers_on_success() {
        let feed = Arc::new(RecordFeed::default());
        let power = Arc::new(FakePower::ok());
        let runner = eclipse_runner().streaming(&["(1/2) upgrading eclipseos-abyss"], Ok(Some(0)));
        let (d, runner) = deps(runner, feed, power.clone());
        let handle = spawn_with(d, Duration::from_secs(3600));
        wait_for(&handle, |u| matches!(u, Update::Pending { .. }));
        handle.apply(After::Reboot);
        wait_for(&handle, |u| matches!(u, Update::Event(Event::Installed { .. })));
        let deadline = Instant::now() + Duration::from_secs(5);
        while power.calls().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(power.calls(), vec![After::Reboot]);
        let pkexec_call = runner
            .calls()
            .into_iter()
            .find(|c| c[0] == PKEXEC)
            .expect("pkexec ran");
        assert_eq!(pkexec_call, argv(&[PKEXEC, HELPER, "all"]));
    }

    #[test]
    fn a_second_apply_while_one_runs_is_refused() {
        let feed = Arc::new(RecordFeed::default());
        let power = Arc::new(FakePower::ok());
        let runner = eclipse_runner()
            .streaming(&[], Ok(Some(1)))
            .delayed(Duration::from_millis(400));
        let (d, _) = deps(runner, feed, power.clone());
        let handle = spawn_with(d, Duration::from_secs(3600));
        wait_for(&handle, |u| matches!(u, Update::Pending { .. }));
        handle.apply(After::PowerOff);
        handle.apply(After::PowerOff);
        let refused = wait_for(&handle, |u| matches!(u, Update::Event(Event::Refused { .. })));
        assert_eq!(
            refused,
            Update::Event(Event::Refused {
                reason: "an update is already running".into()
            })
        );
        wait_for(&handle, |u| matches!(u, Update::Event(Event::Failed { .. })));
        assert!(power.calls().is_empty());
    }

    /// Exercises the real runner against the host's `checkupdates`. Needs
    /// pacman-contrib and network, so it is ignored by default.
    #[test]
    #[ignore = "runs checkupdates on the host"]
    fn real_discovery_runs_on_this_host() {
        let update = discover(&SystemRunner);
        assert!(matches!(update, Update::Pending { .. } | Update::Unavailable));
    }

    #[test]
    fn feed_socket_is_best_effort_when_absent() {
        // No socket path (no runtime dir): sends are dropped, never panic.
        SocketFeed { path: None }.send(FeedRequest::Done { id: JOB_ID.into() });
    }
}
