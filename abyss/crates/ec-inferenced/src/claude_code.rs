// SPDX-License-Identifier: AGPL-3.0-only
//! The Claude Code backend: one warm, sandboxed `claude -p` per task
//! (ADR 0076).
//!
//! ```text
//! request (Messages-API shape)
//!    -> session for req.task (started on first use, kept warm)
//!         claude -p --input-format stream-json --output-format stream-json
//!         every built-in tool off; MCP = the tool shim (src/shim.rs)
//!    <- text blocks / tool_use blocks / a failure
//! ```
//!
//! **Why a session is stateful.** `claude` keeps its own conversation and
//! reasoning, so a follow-up request only carries what is *new*. The session
//! remembers the Messages-API history it has accounted for (`consumed`); a
//! request whose history does not extend that exactly (compared after
//! normalising, so cosmetic differences do not matter) restarts the session
//! and replays the old history as a labelled transcript in the first turn.
//!
//! **Credential.** The OAuth token is fetched from brokerd once per session
//! start and reaches the child only through its *environment*, never through
//! any argv. Under bwrap that is done with a pipe the router writes before
//! spawning: bwrap copies it into `/run/eclipse/oauth-token` inside the
//! sandbox (`--perms 0400 --file <fd>`, so only an fd *number* is in bwrap's
//! argv), and the `--sandbox-init` stage reads it, deletes the file, puts it
//! in its environment and only then applies Landlock and execs `claude`. The
//! token is never on the host filesystem. (In the test-only unsandboxed mode
//! it is set with `Command::env`, which is not argv either.)
//!
//! **Every kill path removes the session directory**: `Close`, idle reap,
//! the child exiting, a failed turn, and router shutdown (`Drop`). A router
//! killed outright leaves directories behind; the next daemon start sweeps
//! them ([`ClaudeCode::sweep_stale`]).

use crate::api::failure;
use crate::credential::Credentials;
use crate::secret::ApiKey;
use crate::shim::{self, lock, Hub, ToolResult, TOOL_PREFIX};
use ec_agentd::sandbox::FsPolicy;
use ec_inference_wire::{Completion, Failure, Request, Value};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::DirBuilder;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

/// Where the router's own binary appears inside the sandbox (it is the
/// `--sandbox-init` helper and the `--tool-shim`).
pub const IN_EXE: &str = "/run/eclipse/ec-inferenced";
/// Where bwrap puts the token inside the sandbox until the init stage reads
/// and deletes it.
pub const IN_TOKEN: &str = "/run/eclipse/oauth-token";
/// The environment variable `claude` reads its login token from.
pub const TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

pub const DEFAULT_MAX_SESSIONS: usize = 8;
pub const DEFAULT_IDLE: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_TURN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// After the first `tool_use` of a turn, how long to wait for siblings of the
/// same parallel call before replying.
pub const DEFAULT_TOOL_BATCH_WAIT: Duration = Duration::from_millis(300);
const REAP_INTERVAL: Duration = Duration::from_secs(30);
/// `claude` takes the system prompt as one argv entry (limit 128 KiB each).
const MAX_SYSTEM: usize = 100 * 1024;
/// Longest single stdout line from `claude` we accept.
const MAX_LINE: u64 = 16 * 1024 * 1024;
/// Longest error text carried into a Failure message.
const MAX_ERR_TEXT: usize = 500;
/// A unix socket path must fit `sun_path` (108 bytes).
const MAX_SOCK_PATH: usize = 100;

/// Everything configurable about the backend. [`ClaudeConfig::from_env`] is
/// the production value; tests build their own (the only way to turn the
/// sandbox off: no environment variable can).
#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    /// Run `claude` under bwrap + Landlock + seccomp. Always true outside
    /// tests.
    pub sandbox: bool,
    /// `$XDG_RUNTIME_DIR/eclipse/inferenced/cc`; `None` when unset.
    pub base_dir: Option<PathBuf>,
    /// `$ECLIPSE_CLAUDE_BIN`, else searched for on `PATH` and `~/.local/bin`.
    pub claude_bin: Option<PathBuf>,
    /// The executable that implements `--sandbox-init` and `--tool-shim`.
    pub self_exe: PathBuf,
    pub max_sessions: usize,
    pub idle: Duration,
    pub reap_interval: Duration,
    pub turn_timeout: Duration,
    pub tool_batch_wait: Duration,
    /// One log line per ignored stream event type.
    pub debug: bool,
    /// Extra environment for the child, unsandboxed mode only (tests).
    pub extra_env: Vec<(String, String)>,
}

impl ClaudeConfig {
    pub fn from_env() -> ClaudeConfig {
        ClaudeConfig {
            sandbox: true,
            base_dir: std::env::var_os("XDG_RUNTIME_DIR")
                .map(|r| PathBuf::from(r).join("eclipse/inferenced/cc")),
            claude_bin: std::env::var_os("ECLIPSE_CLAUDE_BIN").map(PathBuf::from),
            self_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/bin/ec-inferenced")),
            max_sessions: DEFAULT_MAX_SESSIONS,
            idle: DEFAULT_IDLE,
            reap_interval: REAP_INTERVAL,
            turn_timeout: DEFAULT_TURN_TIMEOUT,
            tool_batch_wait: DEFAULT_TOOL_BATCH_WAIT,
            debug: std::env::var_os("ECLIPSE_INFERENCED_DEBUG").is_some(),
            extra_env: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Process control.

/// The OS side of a session: the child and its directory. Killing is
/// idempotent and always removes the directory.
struct Ctl {
    dir: PathBuf,
    child: Mutex<Option<Child>>,
    /// Shared with the shim accept thread, which stops waiting when set.
    dead: Arc<AtomicBool>,
}

impl Ctl {
    fn new(dir: PathBuf) -> Ctl {
        Ctl {
            dir,
            child: Mutex::new(None),
            dead: Arc::new(AtomicBool::new(false)),
        }
    }

    fn kill(&self) {
        self.dead.store(true, Ordering::Release);
        if let Some(mut c) = lock(&self.child).take() {
            // Killing bwrap ends its pid namespace's init, which takes the
            // whole sandbox with it.
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    fn exited(&self) -> bool {
        if self.dead.load(Ordering::Acquire) {
            return true;
        }
        match lock(&self.child).as_mut() {
            Some(c) => !matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }
}

impl Drop for Ctl {
    fn drop(&mut self) {
        self.kill();
    }
}

enum Event {
    Line(Value),
    Exit,
}

struct Key {
    system: String,
    model: String,
    tools: Value,
}

impl Key {
    fn of(req: &Request) -> Key {
        Key {
            system: req.system.clone(),
            model: req.model.clone(),
            tools: req.tools.clone(),
        }
    }

    fn same(&self, o: &Key) -> bool {
        self.system == o.system && self.model == o.model && self.tools == o.tools
    }
}

struct Session {
    ctl: Arc<Ctl>,
    stdin: ChildStdin,
    events: Receiver<Event>,
    hub: Arc<Hub>,
    key: Key,
    /// The Messages-API history this session has accounted for, including the
    /// assistant turn it last returned.
    consumed: Vec<Value>,
    /// `tool_use` ids returned to the agent and not yet answered.
    outstanding: HashSet<String>,
    model: String,
}

impl Session {
    fn send_user(&mut self, text: &str) -> Result<(), Failure> {
        let line = json!({"type": "user", "message": {"role": "user", "content": text}});
        let mut s =
            serde_json::to_string(&line).map_err(|_| failure("bad_request", "turn is not serialisable"))?;
        s.push('\n');
        self.stdin
            .write_all(s.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|_| failure("provider_error", "Claude Code is not accepting input"))
    }
}

/// One task's slot. The session mutex serialises requests for the task; the
/// ctl mutex lets `Close` kill the process without waiting for a request that
/// is blocked on it.
struct Slot {
    session: Mutex<Option<Session>>,
    ctl: Mutex<Option<Arc<Ctl>>>,
    closed: AtomicBool,
    last_used: Mutex<Instant>,
}

impl Slot {
    fn new() -> Arc<Slot> {
        Arc::new(Slot {
            session: Mutex::new(None),
            ctl: Mutex::new(None),
            closed: AtomicBool::new(false),
            last_used: Mutex::new(Instant::now()),
        })
    }

    /// The task is over: kill the process now and refuse further use.
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(c) = lock(&self.ctl).take() {
            c.kill();
        }
    }

    /// Kill the current process (a restart, or a failed turn); the slot stays
    /// usable.
    fn kill_ctl(&self) {
        if let Some(c) = lock(&self.ctl).take() {
            c.kill();
        }
    }

    /// Make `c` the slot's process. False (and `c` killed) when the slot was
    /// closed meanwhile; the closed flag is read under the ctl lock, which
    /// `close` also takes, so the two cannot miss each other.
    fn install(&self, c: Arc<Ctl>) -> bool {
        let mut g = lock(&self.ctl);
        if self.closed.load(Ordering::Acquire) {
            drop(g);
            c.kill();
            return false;
        }
        if let Some(old) = g.replace(c) {
            old.kill();
        }
        true
    }

    fn touch(&self) {
        *lock(&self.last_used) = Instant::now();
    }
}

type Job = Box<dyn FnOnce() + Send>;

struct Spawner {
    tx: Sender<Job>,
}

struct Inner {
    cfg: ClaudeConfig,
    creds: Arc<dyn Credentials>,
    slots: Mutex<HashMap<String, Arc<Slot>>>,
    /// Started on first use: the sweep, the spawner thread and the reaper.
    started: OnceLock<Spawner>,
    seq: AtomicU64,
}

/// The backend. Cheap to create; nothing touches the disk or starts a thread
/// until the first session.
pub struct ClaudeCode {
    inner: Arc<Inner>,
}

impl ClaudeCode {
    pub fn new(cfg: ClaudeConfig, creds: Arc<dyn Credentials>) -> ClaudeCode {
        ClaudeCode {
            inner: Arc::new(Inner {
                cfg,
                creds,
                slots: Mutex::new(HashMap::new()),
                started: OnceLock::new(),
                seq: AtomicU64::new(0),
            }),
        }
    }

    pub fn complete(&self, req: &Request) -> Result<Completion, Failure> {
        self.inner.complete(req)
    }

    /// The task closed: kill its session now.
    pub fn close(&self, task: &str) {
        let slot = lock(&self.inner.slots).remove(task);
        if let Some(slot) = slot {
            slot.close();
            eprintln!("ec-inferenced: claude-code task={task} session closed");
        }
    }

    /// Remove session directories a crashed router left behind. The daemon
    /// calls this once at start-up, before it serves anything; the library
    /// never does it on its own (a test or second instance must not delete a
    /// live router's sessions).
    pub fn sweep_stale(&self) {
        if let Some(base) = &self.inner.cfg.base_dir {
            if let Ok(rd) = std::fs::read_dir(base) {
                for e in rd.flatten() {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }

    /// Live sessions (slots), for tests and diagnostics.
    pub fn session_count(&self) -> usize {
        lock(&self.inner.slots).len()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Router shutdown: no session outlives the router.
        let slots: Vec<Arc<Slot>> = lock(&self.slots).drain().map(|(_, s)| s).collect();
        for s in slots {
            s.close();
        }
    }
}

/// A task id becomes a directory name and a socket path component.
fn valid_task(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Inner {
    fn complete(self: &Arc<Self>, req: &Request) -> Result<Completion, Failure> {
        if !valid_task(&req.task) {
            return Err(failure("bad_request", "malformed task id"));
        }
        let Some(msgs) = req.messages.as_array().filter(|m| !m.is_empty()) else {
            return Err(failure("bad_request", "messages must be a non-empty array"));
        };
        if msgs.last().and_then(|m| m.get("role")).and_then(Value::as_str) != Some("user") {
            return Err(failure("bad_request", "the last message must be a user turn"));
        }
        self.start_threads();
        // A slot can be closed (Close, reaper) between being found and being
        // locked; look again rather than fail the request.
        for _ in 0..3 {
            let slot = self.slot_for(&req.task)?;
            let mut guard = lock(&slot.session);
            if slot.closed.load(Ordering::Acquire) {
                drop(guard);
                self.forget(&req.task, &slot);
                continue;
            }
            let result = self.run(&slot, &mut guard, req);
            slot.touch();
            let empty = guard.is_none();
            drop(guard);
            if empty {
                // Nothing running (the turn or the start failed): free the
                // slot so it does not count against the cap.
                self.forget(&req.task, &slot);
            }
            return result;
        }
        Err(failure(
            "backend_unavailable",
            "the Claude Code session was closed",
        ))
    }

    fn slot_for(&self, task: &str) -> Result<Arc<Slot>, Failure> {
        let mut m = lock(&self.slots);
        if let Some(s) = m.get(task) {
            return Ok(s.clone());
        }
        if m.len() >= self.cfg.max_sessions {
            return Err(failure(
                "backend_unavailable",
                "too many Claude Code sessions are open",
            ));
        }
        let s = Slot::new();
        m.insert(task.to_owned(), s.clone());
        Ok(s)
    }

    /// Remove `slot` from the map if it is still the one registered.
    fn forget(&self, task: &str, slot: &Arc<Slot>) {
        let mut m = lock(&self.slots);
        if m.get(task).is_some_and(|s| Arc::ptr_eq(s, slot)) {
            m.remove(task);
        }
    }

    /// First-use initialisation: the spawner thread and the idle reaper.
    fn start_threads(self: &Arc<Self>) {
        self.started.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            // Children are spawned from this one long-lived thread, never from
            // a request thread: `--die-with-parent` is PR_SET_PDEATHSIG, which
            // fires when the *thread* that spawned the child exits, and
            // request threads are short-lived. It also serialises the token
            // pipe's inheritable fd, so no other child can inherit it.
            let _ = std::thread::Builder::new()
                .name("cc-spawn".into())
                .spawn(move || {
                    for job in rx {
                        job();
                    }
                });
            let weak = Arc::downgrade(self);
            let every = self.cfg.reap_interval;
            let _ = std::thread::Builder::new()
                .name("cc-reaper".into())
                .spawn(move || reaper(weak, every));
            Spawner { tx }
        });
    }

    fn on_spawner<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        let sp = self.started.get()?;
        let (tx, rx) = mpsc::channel();
        sp.tx
            .send(Box::new(move || {
                let _ = tx.send(f());
            }))
            .ok()?;
        rx.recv().ok()
    }

    /// One request against the task's session, starting or restarting it as
    /// needed. `guard` is the slot's session; it is left `None` on failure.
    fn run(&self, slot: &Slot, guard: &mut Option<Session>, req: &Request) -> Result<Completion, Failure> {
        let msgs = req.messages.as_array().map(Vec::as_slice).unwrap_or(&[]);
        let key = Key::of(req);
        let plan = match guard.as_ref() {
            Some(s) => plan_for(s, &key, msgs),
            None => Plan::Fresh,
        };
        if matches!(plan, Plan::Fresh) {
            // The old process must be gone (and its directory removed) before
            // the new one is created.
            *guard = None;
            slot.kill_ctl();
            let session = self.start(slot, req)?;
            *guard = Some(session);
        }
        let Some(s) = guard.as_mut() else {
            return Err(failure("backend_unavailable", "no Claude Code session"));
        };
        let fed = match plan {
            Plan::Fresh => s.send_user(&fresh_turn(msgs)),
            Plan::Text(t) => s.send_user(&t),
            Plan::Results(rs) => {
                for (id, r) in rs {
                    s.hub.deliver(&id, r);
                }
                Ok(())
            }
        };
        let turn = fed.and_then(|()| self.read_turn(s));
        match turn {
            Ok(t) => {
                let (content, stop_reason, input_tokens, output_tokens, cost_usd, ids) = match t {
                    Turn::Tools { content, ids } => (content, "tool_use".to_owned(), 0, 0, None, ids),
                    Turn::Done {
                        content,
                        stop_reason,
                        input_tokens,
                        output_tokens,
                        cost_usd,
                    } => (
                        content,
                        stop_reason,
                        input_tokens,
                        output_tokens,
                        cost_usd,
                        HashSet::new(),
                    ),
                };
                let mut consumed = msgs.to_vec();
                consumed.push(json!({"role": "assistant", "content": content}));
                s.consumed = consumed;
                s.outstanding = ids;
                Ok(Completion {
                    content: Value::Array(content),
                    stop_reason,
                    model: s.model.clone(),
                    input_tokens,
                    output_tokens,
                    cost_usd,
                })
            }
            Err(e) => {
                // The conversation's state is unknown: the next request starts
                // a new session and replays the history.
                *guard = None;
                slot.kill_ctl();
                Err(e)
            }
        }
    }

    /// Build and start a session for `req`'s task. Fresh process, fresh
    /// directory; no turn is sent.
    fn start(&self, slot: &Slot, req: &Request) -> Result<Session, Failure> {
        let base = self
            .cfg
            .base_dir
            .clone()
            .ok_or_else(|| failure("backend_unavailable", "XDG_RUNTIME_DIR is not set"))?;
        let claude = locate_claude(&self.cfg).ok_or_else(|| {
            failure(
                "backend_unavailable",
                "Claude Code is not installed (set ECLIPSE_CLAUDE_BIN)",
            )
        })?;
        if req.system.len() > MAX_SYSTEM {
            return Err(failure(
                "bad_request",
                "the system prompt is too long for Claude Code",
            ));
        }
        // Held only until it has been handed to the child; zeroed on drop.
        let token = self.creds.claude_code_token(&req.package, &req.account)?;

        // `<task>-<n>`: unique per start, so a killed session's cleanup can
        // never remove a newer session's directory.
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("{}-{n}", req.task));
        if dir.join("shim.sock").as_os_str().len() > MAX_SOCK_PATH {
            return Err(failure(
                "backend_unavailable",
                "the runtime directory path is too long",
            ));
        }
        let ctl = Arc::new(Ctl::new(dir.clone()));
        // Register before creating anything: from here every exit path,
        // including `Close`, kills the ctl and so removes the directory.
        if !slot.install(ctl.clone()) {
            return Err(failure(
                "backend_unavailable",
                "the Claude Code session was closed",
            ));
        }
        match self.start_in(&ctl, &claude, token, req) {
            Ok(s) => {
                eprintln!("ec-inferenced: claude-code task={} session started", req.task);
                Ok(s)
            }
            Err(e) => {
                ctl.kill();
                Err(e)
            }
        }
    }

    fn start_in(
        &self,
        ctl: &Arc<Ctl>,
        claude: &Path,
        token: ApiKey,
        req: &Request,
    ) -> Result<Session, Failure> {
        let dir = ctl.dir.clone();
        let io_err = |what: &str, e: io::Error| {
            failure(
                "backend_unavailable",
                format!("cannot prepare the Claude Code session ({what}: {e})"),
            )
        };
        make_dirs(&dir).map_err(|e| io_err("directory", e))?;

        let claude_dir = claude
            .parent()
            .ok_or_else(|| failure("backend_unavailable", "Claude Code has no install directory"))?
            .to_path_buf();
        let sock = dir.join("shim.sock");
        let exe_in = if self.cfg.sandbox {
            PathBuf::from(IN_EXE)
        } else {
            self.cfg.self_exe.clone()
        };
        write_private(
            &dir.join("mcp.json"),
            mcp_config(&exe_in, &sock).to_string().as_bytes(),
        )
        .map_err(|e| io_err("mcp.json", e))?;
        if self.cfg.sandbox {
            let policy = sandbox_policy(&dir, &claude_dir);
            write_private(&dir.join("sandbox.json"), policy.to_json().to_string().as_bytes())
                .map_err(|e| io_err("sandbox.json", e))?;
        }

        // The shim listener exists before `claude` starts, so the shim's
        // connect cannot race it.
        let listener = UnixListener::bind(&sock).map_err(|e| io_err("shim socket", e))?;
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| io_err("shim socket", e))?;
        let hub = Hub::new();
        shim::spawn_accept(
            listener,
            sock,
            rustix::process::getuid(),
            mcp_tools(&req.tools),
            hub.clone(),
            ctl.dead.clone(),
        )
        .map_err(|e| io_err("shim thread", e))?;

        let params = SpawnParams {
            sandbox: self.cfg.sandbox,
            dir,
            claude: claude.to_path_buf(),
            claude_dir,
            self_exe: self.cfg.self_exe.clone(),
            system: req.system.clone(),
            model: req.model.clone(),
            extra_env: self.cfg.extra_env.clone(),
        };
        let spawned = self
            .on_spawner(move || spawn_claude(&params, &token))
            .ok_or_else(|| failure("backend_unavailable", "the Claude Code launcher is not running"))?;
        let mut child = spawned.map_err(|e| {
            failure(
                "backend_unavailable",
                format!("cannot start Claude Code ({e}); is bubblewrap installed?"),
            )
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure("backend_unavailable", "Claude Code has no stdio"));
        };
        let stderr = child.stderr.take();
        let pid = child.id();
        *lock(&ctl.child) = Some(child);
        // A `Close` that landed while we were spawning killed nothing (no
        // child yet): catch it here so the process is not orphaned.
        if ctl.dead.load(Ordering::Acquire) {
            return Err(failure(
                "backend_unavailable",
                "the Claude Code session was closed",
            ));
        }
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("cc-stdout".into())
            .spawn(move || read_events(stdout, tx))
            .map_err(|e| io_err("reader thread", e))?;
        if let Some(e) = stderr {
            let task = req.task.clone();
            let _ = std::thread::Builder::new()
                .name("cc-stderr".into())
                .spawn(move || log_stderr(e, &task, pid));
        }
        Ok(Session {
            ctl: ctl.clone(),
            stdin,
            events: rx,
            hub,
            key: Key::of(req),
            consumed: Vec::new(),
            outstanding: HashSet::new(),
            model: req.model.clone(),
        })
    }

    /// Read stream-json events until the turn yields tool calls or ends.
    fn read_turn(&self, s: &mut Session) -> Result<Turn, Failure> {
        let deadline = Instant::now() + self.cfg.turn_timeout;
        let mut texts: Vec<Value> = Vec::new();
        let mut tool_uses: Vec<Value> = Vec::new();
        let mut grace: Option<Instant> = None;
        loop {
            let now = Instant::now();
            let until = grace.map_or(deadline, |g| g.min(deadline));
            let ev = match s.events.recv_timeout(until.saturating_duration_since(now)) {
                Ok(Event::Line(v)) => v,
                Ok(Event::Exit) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(failure("provider_error", "Claude Code exited unexpectedly"));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if tool_uses.is_empty() {
                        return Err(failure("timeout", "Claude Code did not finish the turn in time"));
                    }
                    return Ok(tools_turn(texts, tool_uses));
                }
            };
            match ev.get("type").and_then(Value::as_str) {
                // `system/init` arrives once per *turn*, not once per
                // process: every one is checked, none means a new session.
                Some("system") => {
                    if ev.get("subtype").and_then(Value::as_str) == Some("init") {
                        check_init(&ev)?;
                        if let Some(m) = ev.get("model").and_then(Value::as_str) {
                            s.model = m.to_owned();
                        }
                    }
                }
                Some("assistant") => {
                    let blocks = ev.pointer("/message/content").and_then(Value::as_array);
                    for b in blocks.into_iter().flatten() {
                        match b.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(t) =
                                    b.get("text").and_then(Value::as_str).filter(|t| !t.is_empty())
                                {
                                    texts.push(json!({"type": "text", "text": t}));
                                }
                            }
                            Some("tool_use") => {
                                if let Some(tu) = strip_tool_use(b) {
                                    tool_uses.push(tu);
                                    if grace.is_none() {
                                        grace = Some(Instant::now() + self.cfg.tool_batch_wait);
                                    }
                                }
                            }
                            // `thinking` is dropped: the session keeps its
                            // own reasoning.
                            _ => {}
                        }
                    }
                }
                Some("result") => return finish(&ev, texts),
                // The `tool_result` echo.
                Some("user") => {}
                // `rate_limit_event` and anything newer: never an error.
                other => {
                    if self.cfg.debug {
                        eprintln!(
                            "ec-inferenced: claude-code ignoring event type={}",
                            other.unwrap_or("?")
                        );
                    }
                }
            }
        }
    }
}

fn reaper(weak: Weak<Inner>, every: Duration) {
    loop {
        std::thread::sleep(every);
        let Some(inner) = weak.upgrade() else { return };
        let all: Vec<(String, Arc<Slot>)> = lock(&inner.slots)
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (task, slot) in all {
            // A request holds the session lock for the whole turn: a busy
            // slot is neither idle nor dead.
            let Ok(guard) = slot.session.try_lock() else {
                continue;
            };
            let idle = lock(&slot.last_used).elapsed() > inner.cfg.idle;
            let dead = lock(&slot.ctl).as_ref().is_some_and(|c| c.exited());
            if idle || dead {
                // Closed under the session lock: a request that was waiting
                // for it sees `closed` and starts over on a fresh slot.
                slot.close();
                drop(guard);
                inner.forget(&task, &slot);
                eprintln!(
                    "ec-inferenced: claude-code task={task} session reaped ({})",
                    if idle { "idle" } else { "exited" }
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Turn results and stream parsing (pure).

enum Turn {
    Tools {
        content: Vec<Value>,
        ids: HashSet<String>,
    },
    Done {
        content: Vec<Value>,
        stop_reason: String,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: Option<f64>,
    },
}

fn tools_turn(texts: Vec<Value>, tool_uses: Vec<Value>) -> Turn {
    let ids = tool_uses
        .iter()
        .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let mut content = texts;
    content.extend(tool_uses);
    Turn::Tools { content, ids }
}

/// A `tool_use` block as the agent should see it: `claude`'s
/// `mcp__eclipse__<name>` back to the agent's `<name>`.
fn strip_tool_use(b: &Value) -> Option<Value> {
    let id = b.get("id").and_then(Value::as_str)?;
    let name = b.get("name").and_then(Value::as_str)?;
    Some(json!({
        "type": "tool_use",
        "id": id,
        "name": name.strip_prefix(TOOL_PREFIX).unwrap_or(name),
        "input": b.get("input").cloned().unwrap_or_else(|| json!({})),
    }))
}

/// ADR 0076: refuse a session that lists any tool other than ours. Anything
/// unexpected about the field is also a refusal.
fn check_init(ev: &Value) -> Result<(), Failure> {
    let ok = ev.get("tools").and_then(Value::as_array).is_some_and(|a| {
        a.iter()
            .all(|t| t.as_str().is_some_and(|n| n.starts_with(TOOL_PREFIX)))
    });
    if ok {
        Ok(())
    } else {
        Err(failure(
            "backend_unavailable",
            "Claude Code exposed built-in tools; refusing",
        ))
    }
}

fn cap(s: &str) -> String {
    s.chars().take(MAX_ERR_TEXT).collect()
}

/// The turn's `result` event, as a completion or a mapped failure.
fn finish(ev: &Value, mut texts: Vec<Value>) -> Result<Turn, Failure> {
    if ev.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        return Err(map_error(ev));
    }
    if texts.is_empty() {
        let t = ev.get("result").and_then(Value::as_str).unwrap_or("");
        texts.push(json!({"type": "text", "text": t}));
    }
    let n = |k: &str| {
        ev.pointer(&format!("/usage/{k}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    Ok(Turn::Done {
        content: texts,
        stop_reason: ev
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_owned(),
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cost_usd: ev.get("total_cost_usd").and_then(Value::as_f64),
    })
}

fn map_error(ev: &Value) -> Failure {
    let text = cap(ev.get("result").and_then(Value::as_str).unwrap_or(""));
    match ev.get("api_error_status").and_then(Value::as_u64) {
        Some(429) => failure("rate_limited", "Claude Code was rate-limited (HTTP 429)"),
        Some(s @ (401 | 403)) => failure(
            "no_credential",
            format!(
                "Claude Code rejected the stored token (HTTP {s}): sign in again in Settings → Accounts, or run `ec-secret account login <account>`"
            ),
        ),
        Some(s) => failure("provider_error", format!("Claude Code failed (HTTP {s}): {text}")),
        None => failure("provider_error", format!("Claude Code failed: {text}")),
    }
}

// ---------------------------------------------------------------------------
// Mapping a request onto the session (pure).

enum Plan {
    /// Start (or restart) a session; replay the history as a transcript.
    Fresh,
    /// A new user text turn on the live session.
    Text(String),
    /// The agent's results for the calls the session is waiting on.
    Results(Vec<(String, ToolResult)>),
}

fn blocks_of(m: &Value) -> Vec<Value> {
    match m.get("content") {
        Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

/// A message reduced to what identifies it, so a replayed history compares
/// equal whatever extra fields a client added.
fn norm_msg(m: &Value) -> Value {
    let blocks: Vec<Value> = blocks_of(m)
        .iter()
        .map(|b| {
            let mut o = serde_json::Map::new();
            for k in [
                "type",
                "text",
                "id",
                "name",
                "input",
                "tool_use_id",
                "content",
                "is_error",
            ] {
                if let Some(v) = b.get(k) {
                    o.insert(k.to_owned(), v.clone());
                }
            }
            Value::Object(o)
        })
        .collect();
    json!({"role": m.get("role").cloned().unwrap_or(Value::Null), "content": blocks})
}

/// Text of a `tool_result`'s `content` (a string or blocks).
fn result_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|b| match b.get("text").and_then(Value::as_str) {
                Some(t) if b.get("type").and_then(Value::as_str) == Some("text") => t.to_owned(),
                _ => "[non-text content omitted]".to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn plan_for(s: &Session, key: &Key, msgs: &[Value]) -> Plan {
    let n = s.consumed.len();
    // A changed tool list, system prompt or model cannot be applied to a
    // running process (all three are fixed when it starts), and history that
    // does not extend ours by exactly one user turn is not an append.
    if s.ctl.exited()
        || !s.key.same(key)
        || msgs.len() != n + 1
        || !s
            .consumed
            .iter()
            .zip(msgs)
            .all(|(a, b)| norm_msg(a) == norm_msg(b))
    {
        return Plan::Fresh;
    }
    let last = &msgs[n];
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return Plan::Fresh;
    }
    let blocks = blocks_of(last);
    let mut results = Vec::new();
    let mut texts = Vec::new();
    for b in &blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("tool_result") => {
                let Some(id) = b.get("tool_use_id").and_then(Value::as_str) else {
                    return Plan::Fresh;
                };
                results.push((
                    id.to_owned(),
                    ToolResult {
                        text: result_text(b.get("content").unwrap_or(&Value::Null)),
                        is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    },
                ));
            }
            Some("text") => texts.push(b.get("text").and_then(Value::as_str).unwrap_or("").to_owned()),
            _ => return Plan::Fresh,
        }
    }
    if !results.is_empty() && texts.is_empty() && !s.outstanding.is_empty() {
        let ids: HashSet<&str> = results.iter().map(|(i, _)| i.as_str()).collect();
        let want: HashSet<&str> = s.outstanding.iter().map(String::as_str).collect();
        if ids == want && ids.len() == results.len() {
            return Plan::Results(results);
        }
    } else if results.is_empty() && !texts.is_empty() && s.outstanding.is_empty() {
        return Plan::Text(texts.join("\n\n"));
    }
    // Anything else (results nobody asked for, calls left unanswered, text
    // mixed with results) cannot be fed to the live session faithfully.
    Plan::Fresh
}

/// The first user turn of a fresh session: the history before the last user
/// turn as a labelled transcript, then the last turn itself.
fn fresh_turn(msgs: &[Value]) -> String {
    let Some((last, prior)) = msgs.split_last() else {
        return String::new();
    };
    let blocks = blocks_of(last);
    let plain_text = !blocks.is_empty()
        && blocks
            .iter()
            .all(|b| b.get("type").and_then(Value::as_str) == Some("text"));
    if plain_text {
        let text = blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");
        if prior.is_empty() {
            return text;
        }
        return format!("{}\n\n{text}", transcript(prior));
    }
    format!(
        "{}\n\nContinue from where the conversation left off.",
        transcript(msgs)
    )
}

fn transcript(msgs: &[Value]) -> String {
    let mut out = String::from(
        "[Earlier conversation, replayed because the session restarted. It is your own history; continue from it.]\n",
    );
    for m in msgs {
        let who = if m.get("role").and_then(Value::as_str) == Some("assistant") {
            "Assistant"
        } else {
            "User"
        };
        for b in blocks_of(m) {
            let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
            match b.get("type").and_then(Value::as_str) {
                Some("text") => out.push_str(&format!("{who}: {}\n", s("text"))),
                Some("tool_use") => out.push_str(&format!(
                    "Assistant called tool {} with input {}\n",
                    s("name"),
                    b.get("input").cloned().unwrap_or(Value::Null)
                )),
                Some("tool_result") => out.push_str(&format!(
                    "Tool result for {}{}: {}\n",
                    s("tool_use_id"),
                    if b.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                        " (error)"
                    } else {
                        ""
                    },
                    result_text(b.get("content").unwrap_or(&Value::Null))
                )),
                Some("thinking" | "redacted_thinking") | None => {}
                Some(t) => out.push_str(&format!("{who}: [{t} block omitted]\n")),
            }
        }
    }
    out.push_str("[End of earlier conversation]");
    out
}

/// The agent's `tools` (Messages-API: `input_schema`) as MCP tool
/// definitions (`inputSchema`).
fn mcp_tools(tools: &Value) -> Value {
    Value::Array(
        tools
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter_map(|t| {
                let name = t.get("name").and_then(Value::as_str)?;
                Some(json!({
                    "name": name,
                    "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                    "inputSchema": t.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"})),
                }))
            })
            .collect(),
    )
}

fn mcp_config(exe: &Path, sock: &Path) -> Value {
    json!({"mcpServers": {"eclipse": {
        "type": "stdio",
        "command": exe.to_string_lossy(),
        "args": ["--tool-shim", sock.to_string_lossy()],
    }}})
}

// ---------------------------------------------------------------------------
// Files, locating claude, spawning.

fn is_exec_file(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `$ECLIPSE_CLAUDE_BIN`, else the first `claude` on `PATH`, else
/// `~/.local/bin/claude`; resolved through symlinks (the native installer
/// keeps one file per version beside a `claude` link).
fn locate_claude(cfg: &ClaudeConfig) -> Option<PathBuf> {
    let cand = match &cfg.claude_bin {
        Some(p) => p.clone(),
        None => std::env::var_os("PATH")
            .and_then(|p| {
                std::env::split_paths(&p)
                    .map(|d| d.join("claude"))
                    .find(|c| is_exec_file(c))
            })
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin/claude")))?,
    };
    let real = std::fs::canonicalize(cand).ok()?;
    is_exec_file(&real).then_some(real)
}

/// The session directory and its private subdirectories, all 0700.
fn make_dirs(dir: &Path) -> io::Result<()> {
    let mut b = DirBuilder::new();
    b.recursive(true).mode(0o700);
    b.create(dir)?;
    for sub in ["config", "home", "work"] {
        b.create(dir.join(sub))?;
    }
    Ok(())
}

fn write_private(p: &Path, b: &[u8]) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(p)?
        .write_all(b)
}

/// What the sandboxed `claude` may touch: the system, its own install
/// directory, our binary, and its session directory. Everything else is
/// denied, whatever bwrap's mounts would have allowed.
fn sandbox_policy(dir: &Path, claude_dir: &Path) -> FsPolicy {
    let p = |s: &[&str]| s.iter().map(PathBuf::from).collect::<Vec<_>>();
    let mut ro_exec = p(&["/usr", "/lib", "/lib32", "/lib64", "/bin", "/sbin"]);
    ro_exec.push(claude_dir.to_path_buf());
    ro_exec.push(PathBuf::from(IN_EXE));
    FsPolicy {
        ro_exec,
        // TLS roots, resolv.conf, nsswitch and the like live in /etc.
        ro: p(&[
            "/proc",
            "/sys",
            "/etc",
            "/dev/urandom",
            "/dev/random",
            "/dev/zero",
        ]),
        rw: vec![dir.to_path_buf(), PathBuf::from("/tmp")],
        rw_dev: p(&["/dev/null"]),
    }
}

struct SpawnParams {
    sandbox: bool,
    dir: PathBuf,
    /// The resolved `claude` binary.
    claude: PathBuf,
    /// Its directory, which the sandbox binds read-only.
    claude_dir: PathBuf,
    self_exe: PathBuf,
    system: String,
    model: String,
    extra_env: Vec<(String, String)>,
}

/// `claude`'s own arguments (after the sandbox wrapper, if any).
fn claude_args(p: &SpawnParams) -> Vec<OsString> {
    let mut a: Vec<OsString> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    a.push(p.dir.join("mcp.json").into_os_string());
    a.push("--setting-sources".into());
    a.push("".into());
    if !p.system.is_empty() {
        a.push("--system-prompt".into());
        a.push(p.system.clone().into());
    }
    a.push("--model".into());
    a.push(p.model.clone().into());
    // Nothing is left for it to ask about: every action is the agent's.
    a.push("--dangerously-skip-permissions".into());
    a
}

/// The bwrap invocation. The token is *not* in it: `token_fd` is only the
/// number of the pipe bwrap copies into [`IN_TOKEN`].
fn bwrap_command(p: &SpawnParams, token_fd: i32) -> Command {
    let d = &p.dir;
    let mut c = Command::new("bwrap");
    c.args([
        "--unshare-all",
        "--share-net",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--ro-bind",
        "/",
        "/",
        "--tmpfs",
        "/home",
        "--tmpfs",
        "/tmp",
        "--tmpfs",
        "/run",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
    ]);
    c.arg("--bind").arg(d).arg(d);
    c.arg("--ro-bind").arg(&p.claude_dir).arg(&p.claude_dir);
    c.arg("--ro-bind").arg(&p.self_exe).arg(IN_EXE);
    c.args(["--perms", "0400", "--file"])
        .arg(token_fd.to_string())
        .arg(IN_TOKEN);
    c.arg("--setenv").arg("HOME").arg(d.join("home"));
    c.arg("--setenv").arg("CLAUDE_CONFIG_DIR").arg(d.join("config"));
    c.args(["--setenv", "PATH", "/usr/bin:/bin"]);
    c.args(["--setenv", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"]);
    c.args(["--setenv", "DISABLE_AUTOUPDATER", "1"]);
    c.arg("--chdir").arg(d.join("work")).arg("--");
    c.arg(IN_EXE)
        .arg("--sandbox-init")
        .arg(d.join("sandbox.json"))
        .arg("--token-file")
        .arg(IN_TOKEN)
        .arg("--");
    c.arg(&p.claude).args(claude_args(p));
    c
}

/// Unsandboxed child (tests only: [`ClaudeConfig::sandbox`] is false).
fn direct_command(p: &SpawnParams) -> Command {
    let mut c = Command::new(&p.claude);
    c.args(claude_args(p))
        .env_clear()
        .env("HOME", p.dir.join("home"))
        .env("CLAUDE_CONFIG_DIR", p.dir.join("config"))
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .envs(p.extra_env.iter().map(|(k, v)| (k, v)))
        .current_dir(p.dir.join("work"));
    c
}

/// Runs on the spawner thread. Hands the token to the child and returns it
/// running with piped stdin/stdout.
fn spawn_claude(p: &SpawnParams, token: &ApiKey) -> io::Result<Child> {
    if !p.sandbox {
        let mut c = direct_command(p);
        c.env(TOKEN_ENV, token.expose());
        return c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
    }
    // A pipe, not argv and not a file on the host: the token goes in here, the
    // read end is inherited by bwrap (the fd is deliberately not
    // close-on-exec), and bwrap copies it into the sandbox's own tmpfs.
    let (r, w) = rustix::pipe::pipe().map_err(io::Error::from)?;
    rustix::io::write(&w, token.expose().as_bytes()).map_err(io::Error::from)?;
    drop(w);
    let child = bwrap_command(p, r.as_raw_fd())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    drop(r);
    child
}

/// `claude`'s stdout, one JSON object per line, until it closes.
fn read_events(stdout: ChildStdout, tx: Sender<Event>) {
    let mut r = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = match (&mut r).take(MAX_LINE + 1).read_until(b'\n', &mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 || buf.len() as u64 > MAX_LINE {
            break;
        }
        // A line that is not JSON is not an event; ignore it.
        if let Ok(v) = serde_json::from_slice::<Value>(&buf) {
            if tx.send(Event::Line(v)).is_err() {
                return;
            }
        }
    }
    let _ = tx.send(Event::Exit);
}

/// bwrap's and the sandbox helper's own stderr (the agent's is /dev/null'd
/// by the helper): the first 2 KiB, logged once. Never holds the token.
fn log_stderr(mut e: ChildStderr, task: &str, pid: u32) {
    let mut head = Vec::new();
    let _ = (&mut e).take(2048).read_to_end(&mut head);
    let text = String::from_utf8_lossy(&head);
    if !text.trim().is_empty() {
        eprintln!(
            "ec-inferenced: claude-code task={task} pid={pid} launcher: {}",
            text.trim()
        );
    }
    let _ = io::copy(&mut e, &mut io::sink());
}

/// `--sandbox-init <cfg> [--token-file <path>] -- <prog> [args...]`, run
/// inside bwrap. Reads the token, deletes its file, puts it in the
/// environment, then hands over to the agents' second stage (Landlock,
/// seccomp, exec). Never returns.
pub fn sandbox_init(cfg: &str, token_file: Option<&str>, argv: &[String]) -> ! {
    if let Some(path) = token_file {
        let read = std::fs::read(path).map(zeroize::Zeroizing::new);
        // Gone before Landlock is applied and before `claude` exists. If the
        // delete fails, Landlock still denies `claude` any read of /run.
        let _ = std::fs::remove_file(path);
        match read.ok().and_then(|b| ApiKey::from_bytes(&b)) {
            Some(k) => std::env::set_var(TOKEN_ENV, k.expose()),
            None => {
                eprintln!("ec-inferenced sandbox-init: no usable token");
                std::process::exit(ec_agentd::sandbox::EXIT_SANDBOX);
            }
        }
    }
    ec_agentd::sandbox::init_main(cfg, argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> SpawnParams {
        SpawnParams {
            sandbox: true,
            dir: PathBuf::from("/run/user/1000/eclipse/inferenced/cc/T-1"),
            claude: PathBuf::from("/home/u/.local/share/claude/versions/2.0.1"),
            claude_dir: PathBuf::from("/home/u/.local/share/claude/versions"),
            self_exe: PathBuf::from("/usr/bin/ec-inferenced"),
            system: "be brief".into(),
            model: "claude-opus-5-5".into(),
            extra_env: Vec::new(),
        }
    }

    fn args(c: &Command) -> Vec<String> {
        c.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn bwrap_carries_no_token_and_the_baseline_holds() {
        let c = bwrap_command(&params(), 42);
        assert_eq!(c.get_program(), "bwrap");
        let a = args(&c);
        let has = |w: &[&str]| a.windows(w.len()).any(|x| x == w);
        for f in [
            "--unshare-all",
            "--share-net",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ] {
            assert!(a.iter().any(|x| x == f), "{f}");
        }
        assert!(has(&["--cap-drop", "ALL"]));
        // Only an fd number is on the command line, never the value, and no
        // --setenv names the token variable.
        assert!(has(&["--perms", "0400", "--file", "42", IN_TOKEN]));
        assert!(!a.iter().any(|x| x.contains("OAUTH") || x.contains("sk-ant")));
        let setenv: Vec<&str> = a
            .windows(3)
            .filter(|w| w[0] == "--setenv")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(
            setenv,
            [
                "HOME",
                "CLAUDE_CONFIG_DIR",
                "PATH",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
                "DISABLE_AUTOUPDATER"
            ]
        );
        assert!(has(&["--ro-bind", "/usr/bin/ec-inferenced", IN_EXE]));
        // The init stage comes right before claude, with the token file.
        let i = a.iter().position(|x| x == "--sandbox-init").unwrap();
        assert_eq!(a[i - 1], IN_EXE);
        assert_eq!(a[i + 2], "--token-file");
        assert_eq!(a[i + 3], IN_TOKEN);
        assert_eq!(a[i + 4], "--");
        assert_eq!(a[i + 5], "/home/u/.local/share/claude/versions/2.0.1");
    }

    #[test]
    fn claude_is_started_with_every_tool_off() {
        let a: Vec<String> = claude_args(&params())
            .iter()
            .map(|x| x.to_string_lossy().into_owned())
            .collect();
        let has = |w: &[&str]| a.windows(w.len()).any(|x| x == w);
        assert!(has(&["--tools", ""]));
        assert!(has(&["--setting-sources", ""]));
        assert!(has(&[
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json"
        ]));
        assert!(a.iter().any(|x| x == "--strict-mcp-config"));
        assert!(a.iter().any(|x| x == "--dangerously-skip-permissions"));
        assert!(has(&["--system-prompt", "be brief"]));
        assert!(has(&["--model", "claude-opus-5-5"]));
        let mut p = params();
        p.system.clear();
        assert!(!claude_args(&p).iter().any(|x| x == "--system-prompt"));
    }

    #[test]
    fn the_policy_grants_only_the_session_dir_and_scratch_for_writing() {
        let p = sandbox_policy(Path::new("/run/u/cc/T-1"), Path::new("/c/versions"));
        assert_eq!(p.rw, [PathBuf::from("/run/u/cc/T-1"), PathBuf::from("/tmp")]);
        assert!(p.ro_exec.contains(&PathBuf::from("/c/versions")));
        assert!(p.ro_exec.contains(&PathBuf::from(IN_EXE)));
        assert!(!p
            .ro
            .iter()
            .chain(&p.ro_exec)
            .any(|x| x == Path::new("/home") || x == Path::new("/run")));
    }

    #[test]
    fn the_tool_prefix_matches_the_mcp_server_name() {
        assert_eq!(TOOL_PREFIX, format!("mcp__{}__", shim::SERVER_NAME));
        let cfg = mcp_config(Path::new("/x/ec-inferenced"), Path::new("/d/shim.sock"));
        assert_eq!(
            cfg["mcpServers"][shim::SERVER_NAME]["args"],
            json!(["--tool-shim", "/d/shim.sock"])
        );
    }

    #[test]
    fn task_ids_are_directory_safe() {
        assert!(valid_task("01HZXK3M-abc_9"));
        assert!(!valid_task(""));
        assert!(!valid_task("../x"));
        assert!(!valid_task("a/b"));
        assert!(!valid_task(&"a".repeat(65)));
    }

    #[test]
    fn init_with_a_builtin_tool_is_refused() {
        let ok =
            json!({"type": "system", "subtype": "init", "tools": ["mcp__eclipse__a", "mcp__eclipse__b"]});
        assert!(check_init(&ok).is_ok());
        assert!(check_init(&json!({"tools": []})).is_ok());
        for bad in [
            json!({"tools": ["mcp__eclipse__a", "Bash"]}),
            json!({"tools": ["mcp__other__a"]}),
            json!({"tools": [{"name": "mcp__eclipse__a"}]}),
            json!({}),
        ] {
            let f = check_init(&bad).unwrap_err();
            assert_eq!(f.kind, "backend_unavailable");
            assert_eq!(f.message, "Claude Code exposed built-in tools; refusing");
        }
    }

    #[test]
    fn result_errors_map() {
        let e = |status: Value| json!({"type": "result", "is_error": true, "api_error_status": status, "result": "x"});
        assert_eq!(map_error(&e(json!(429))).kind, "rate_limited");
        assert_eq!(map_error(&e(json!(401))).kind, "no_credential");
        assert_eq!(map_error(&e(json!(403))).kind, "no_credential");
        assert_eq!(map_error(&e(json!(500))).kind, "provider_error");
        assert_eq!(map_error(&e(Value::Null)).kind, "provider_error");
        let long = json!({"is_error": true, "result": "y".repeat(5000)});
        assert!(map_error(&long).message.len() < 600);
    }

    #[test]
    fn a_result_becomes_a_completion() {
        let ev = json!({
            "type": "result", "subtype": "success", "is_error": false, "result": "fallback",
            "total_cost_usd": 0.25, "usage": {"input_tokens": 7, "output_tokens": 3},
        });
        let Ok(Turn::Done {
            content,
            stop_reason,
            input_tokens,
            output_tokens,
            cost_usd,
        }) = finish(&ev, Vec::new())
        else {
            panic!("not done");
        };
        assert_eq!(content, vec![json!({"type": "text", "text": "fallback"})]);
        assert_eq!(stop_reason, "end_turn");
        assert_eq!((input_tokens, output_tokens, cost_usd), (7, 3, Some(0.25)));
        let ev = json!({"is_error": false, "stop_reason": "max_tokens"});
        let Ok(Turn::Done {
            stop_reason,
            cost_usd,
            ..
        }) = finish(&ev, vec![json!({"type": "text", "text": "a"})])
        else {
            panic!("not done");
        };
        assert_eq!((stop_reason.as_str(), cost_usd), ("max_tokens", None));
    }

    #[test]
    fn tool_names_lose_their_prefix() {
        let b = json!({"type": "tool_use", "id": "toolu_1", "name": "mcp__eclipse__screen.capture", "input": {"a": 1}});
        assert_eq!(
            strip_tool_use(&b).unwrap(),
            json!({"type": "tool_use", "id": "toolu_1", "name": "screen.capture", "input": {"a": 1}})
        );
    }

    #[test]
    fn tools_become_mcp_definitions() {
        let t = json!([{"name": "a", "description": "d", "input_schema": {"type": "object", "x": 1}}, {"name": "b"}]);
        assert_eq!(
            mcp_tools(&t),
            json!([
                {"name": "a", "description": "d", "inputSchema": {"type": "object", "x": 1}},
                {"name": "b", "description": "", "inputSchema": {"type": "object"}},
            ])
        );
    }

    #[test]
    fn normalising_ignores_cosmetic_differences() {
        let a = json!({"role": "assistant", "content": "hi"});
        let b = json!({"role": "assistant", "content": [{"type": "text", "text": "hi", "citations": null}]});
        assert_eq!(norm_msg(&a), norm_msg(&b));
        assert_ne!(norm_msg(&a), norm_msg(&json!({"role": "user", "content": "hi"})));
    }

    #[test]
    fn a_fresh_turn_replays_history_as_a_labelled_transcript() {
        let one = vec![json!({"role": "user", "content": "hello"})];
        assert_eq!(fresh_turn(&one), "hello");
        let h = vec![
            json!({"role": "user", "content": "q1"}),
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "a1"},
                {"type": "tool_use", "id": "t1", "name": "look", "input": {"x": 1}},
            ]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "seen"}]}),
            json!({"role": "assistant", "content": "a2"}),
            json!({"role": "user", "content": "q2"}),
        ];
        let t = fresh_turn(&h);
        assert!(t.starts_with("[Earlier conversation"));
        for want in [
            "User: q1",
            "Assistant: a1",
            "Assistant called tool look with input {\"x\":1}",
            "Tool result for t1: seen",
            "Assistant: a2",
            "[End of earlier conversation]\n\nq2",
        ] {
            assert!(t.contains(want), "{want} in {t}");
        }
        assert!(!t.contains("hmm"));
        // A history that ends in tool results has no plain last turn.
        let tr = &h[..3];
        let t = fresh_turn(tr);
        assert!(t.ends_with("Continue from where the conversation left off."));
        assert!(t.contains("Tool result for t1: seen"));
    }
}
