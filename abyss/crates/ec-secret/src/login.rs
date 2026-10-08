// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-secret account login <account>`: sign a Claude account in and keep the
//! long-lived OAuth token as `claude-code-token[.account]`.
//!
//! It runs `claude setup-token` on a pseudo-terminal (500 columns wide, so the
//! token never wraps) with a fresh private `CLAUDE_CONFIG_DIR` and
//! `BROWSER=true`, so nothing opens and nothing lands in the user's own
//! `~/.claude`. The authorize URL is read from the output and handed to the
//! caller; the human signs in in a browser and pastes the code back, which is
//! written to the pty. The token is scraped from the output (`scan`), stored
//! through brokerd, and never printed, logged or kept past this function.
//!
//! `--json` writes one event object per line on stdout (`url`, `code_needed`,
//! `stored`, `error`) and reads the code as one line from stdin. Human mode
//! prints the URL, tries `xdg-open`, and asks for the code with echo off.
//!
//! In `--json` mode stdin is read from the start: EOF before a code is written
//! cancels the login. Exit paths: the child's process group is killed and the temp directory
//! removed by drop guards, whichever way this returns (success, error,
//! timeout, SIGINT/SIGTERM/SIGHUP, panic).
//!
//! Environment (tests only): `EC_SECRET_CLAUDE=<path>` runs that program
//! instead of `claude` from `PATH`; `EC_SECRET_LOGIN_TIMEOUT_MS` shortens the
//! 10 minute overall limit.

use crate::accounts::{self, BINDING, TOKEN_BASE};
use crate::scan::{Event, Scanner};
use crate::tty::{self, InputError, Line};
use crate::{sig, REFUSED, USAGE};
use ec_brokerd::bind::Binding;
use ec_brokerd::broker::Status;
use ec_brokerd::wire::{Request, Response, Secret};
use rustix::fs::{open, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{kill_process_group, Pid, Signal};
use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
use rustix::termios::{tcgetattr, tcsetattr, tcsetwinsize, LocalModes, OptionalActions, Winsize};
use std::os::fd::OwnedFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const TIMEOUT: Duration = Duration::from_secs(600);
const COLUMNS: u16 = 500;
/// Between the code and its Enter, so a TUI does not read them as one paste.
const ENTER_DELAY: Duration = Duration::from_millis(200);

pub struct Args {
    pub account: String,
    pub json: bool,
}

/// What the login says, in the chosen format.
struct Out {
    json: bool,
}

impl Out {
    fn event(&self, line: String) {
        if self.json {
            println!("{line}");
        }
    }

    fn url(&self, url: &str) {
        if self.json {
            self.event(format!("{{\"event\":\"url\",\"url\":{}}}", crate::json_str(url)));
        } else {
            eprintln!("Open this address in a browser and sign in:\n\n  {url}\n");
            // Best effort: a missing xdg-open or a headless box is fine.
            let _ = Command::new("xdg-open")
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    fn code_needed(&self) {
        self.event("{\"event\":\"code_needed\"}".into());
    }

    fn stored(&self, account: &str) -> u8 {
        if self.json {
            self.event(format!(
                "{{\"event\":\"stored\",\"account\":{}}}",
                crate::json_str(account)
            ));
        } else {
            println!("stored the Claude Code token for account {account}");
        }
        crate::OK
    }

    fn error(&self, code: &str, message: &str, exit: u8) -> u8 {
        if self.json {
            self.event(format!(
                "{{\"event\":\"error\",\"code\":{},\"message\":{}}}",
                crate::json_str(code),
                crate::json_str(message)
            ));
        } else {
            eprintln!("ec-secret: {message}");
        }
        exit
    }
}

pub fn run(conn: &crate::client::Conn, a: &Args) -> u8 {
    let out = Out { json: a.json };
    let Some(name) = accounts::secret_name(TOKEN_BASE, &a.account) else {
        return out.error(
            "bad_account",
            "an account name is 1 to 32 of A-Z a-z 0-9 _ -",
            USAGE,
        );
    };
    match crate::state(conn) {
        Ok(s) if !s.initialised => {
            return out.error(
                "uninitialised",
                "the store is not initialised: run ec-secret init",
                REFUSED,
            )
        }
        Ok(s) if !s.unlocked => {
            return out.error(
                "locked",
                "brokerd is locked: unlock it first (ec-secret unlock)",
                REFUSED,
            )
        }
        Ok(_) => {}
        Err(f) => return fail(&out, f),
    }
    let Some(claude) = find_claude() else {
        return out.error(
            "no_claude",
            "the `claude` command is not installed (it comes with Claude Code)",
            REFUSED,
        );
    };
    sig::install();
    let token = match scrape(&out, &claude) {
        Ok(t) => t,
        Err((code, msg)) => return out.error(code, &msg, REFUSED),
    };
    store(conn, &out, &a.account, &name, &token)
}

fn fail(out: &Out, f: crate::Fail) -> u8 {
    match f {
        crate::Fail::Transport(_) => out.error("unavailable", "cannot reach brokerd", crate::UNREACHABLE),
        crate::Fail::Input(e) => out.error("refused", e.message(), USAGE),
        crate::Fail::Refused(c) if Status::from_code(c) == Some(Status::BrokerLocked) => out.error(
            "locked",
            "brokerd is locked: unlock it first (ec-secret unlock)",
            REFUSED,
        ),
        crate::Fail::Refused(c) => {
            let (n, _) = crate::status_name(c);
            out.error("refused", &format!("brokerd refused: {n}"), REFUSED)
        }
    }
}

/// Add the token, or rotate it when the secret is already there.
fn store(conn: &crate::client::Conn, out: &Out, account: &str, name: &str, token: &Zeroizing<String>) -> u8 {
    let exists = match conn.call(&Request::List) {
        Ok(Response::List(items)) => items.iter().any(|i| i.name == name),
        Ok(Response::Err(c)) => return fail(out, crate::Fail::Refused(c)),
        Ok(_) => return fail(out, crate::Fail::Transport(crate::client::ClientError::Protocol)),
        Err(e) => return fail(out, crate::Fail::Transport(e)),
    };
    let req = if exists {
        Request::Rotate {
            name: name.to_owned(),
            value: Secret::new(token.as_bytes().to_vec()),
        }
    } else {
        let b = Binding::parse(BINDING).expect("a constant binding");
        crate::add_request(name, vec![b], token.as_bytes())
    };
    match conn.call(&req) {
        Ok(Response::Ok) => out.stored(account),
        Ok(Response::Err(c)) => fail(out, crate::Fail::Refused(c)),
        Ok(_) => fail(out, crate::Fail::Transport(crate::client::ClientError::Protocol)),
        Err(e) => fail(out, crate::Fail::Transport(e)),
    }
}

// ---- finding and running claude -------------------------------------------

fn find_claude() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EC_SECRET_CLAUDE") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join("claude"))
        .find(|p| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

fn timeout() -> Duration {
    std::env::var("EC_SECRET_LOGIN_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(TIMEOUT, Duration::from_millis)
}

/// A private 0700 directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn create() -> std::io::Result<TempDir> {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = base.join(format!("ec-secret-login-{}-{nanos}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(TempDir(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The child and everything it started, killed on drop.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = kill_process_group(Pid::from_child(&self.0), Signal::KILL);
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

enum Msg {
    Out(Zeroizing<Vec<u8>>),
    Eof,
    Code(Result<Line, InputError>),
}

fn pe(what: &'static str) -> impl Fn(Errno) -> String {
    move |e| format!("cannot open a terminal for claude: {what}: {e}")
}

fn pty() -> Result<(OwnedFd, OwnedFd), String> {
    let master =
        openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).map_err(pe("openpt"))?;
    grantpt(&master).map_err(pe("grantpt"))?;
    unlockpt(&master).map_err(pe("unlockpt"))?;
    let path = ptsname(&master, Vec::new()).map_err(pe("ptsname"))?;
    let slave = open(
        path.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(pe("open"))?;
    tcsetwinsize(
        &master,
        Winsize {
            ws_row: 50,
            ws_col: COLUMNS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .map_err(pe("winsize"))?;
    // The pty must not echo the pasted code back into the output we scan.
    if let Ok(mut t) = tcgetattr(&slave) {
        t.local_modes.remove(LocalModes::ECHO);
        let _ = tcsetattr(&slave, OptionalActions::Now, &t);
    }
    Ok((master, slave))
}

fn reader(fd: OwnedFd, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let mut buf = Zeroizing::new([0u8; 4096]);
        loop {
            match rustix::io::read(&fd, &mut buf[..]) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Msg::Out(Zeroizing::new(buf[..n].to_vec()))).is_err() {
                        return;
                    }
                }
                Err(Errno::INTR) => {}
                // EIO is how a pty master reports the slave closing.
                Err(_) => break,
            }
        }
        let _ = tx.send(Msg::Eof);
    });
}

fn write_all(fd: &OwnedFd, mut b: &[u8]) -> Result<(), Errno> {
    while !b.is_empty() {
        match rustix::io::write(fd, b) {
            Ok(n) => b = &b[n..],
            Err(Errno::INTR) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Runs `claude setup-token` to the token. `Err((code, message))` is the
/// error event.
fn scrape(out: &Out, claude: &std::path::Path) -> Result<Zeroizing<String>, (&'static str, String)> {
    let cfg = TempDir::create().map_err(|e| ("refused", format!("cannot make a private directory: {e}")))?;
    let (master, slave) = pty().map_err(|m| ("refused", m))?;
    let dup = |f: &OwnedFd| {
        f.try_clone()
            .map_err(|e| ("refused", format!("cannot set up claude's terminal: {e}")))
    };
    let mut cmd = Command::new(claude);
    cmd.arg("setup-token")
        .env("CLAUDE_CONFIG_DIR", &cfg.0)
        .env("BROWSER", "true")
        .env("TERM", "xterm-256color")
        .env("COLUMNS", COLUMNS.to_string())
        // A credential in the environment would make `claude` skip the login.
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .stdin(Stdio::from(dup(&slave)?))
        .stdout(Stdio::from(dup(&slave)?))
        .stderr(Stdio::from(slave))
        .process_group(0);
    // If this process is killed outright, `claude` goes with it.
    sig::die_with_parent(&mut cmd);
    let child = cmd
        .spawn()
        .map_err(|e| ("no_claude", format!("cannot run {}: {e}", claude.display())))?;
    // Drop the Command (and its copies of the slave) so EOF reaches the reader
    // when the child is gone.
    drop(cmd);
    let _reaper = Reaper(child);

    let (tx, rx) = channel();
    reader(dup(&master)?, tx.clone());
    let deadline = Instant::now() + timeout();
    let mut scanner = Scanner::new();
    let mut code_pending = false;
    // A code that arrived before the prompt (json mode reads stdin from the
    // start, so EOF ends the login even before `claude` asks).
    let mut early: Option<Line> = None;
    let mut prompted = false;
    if out.json {
        ask_code(out, tx.clone());
    }
    let result = loop {
        if sig::requested() {
            break Err(("cancelled", "cancelled".to_owned()));
        }
        let now = Instant::now();
        if now >= deadline {
            break Err(("timeout", "the sign-in took too long (10 minutes)".to_owned()));
        }
        let (events, eof) = match rx.recv_timeout((deadline - now).min(Duration::from_millis(100))) {
            Ok(Msg::Out(chunk)) => (scanner.feed(&chunk), false),
            Ok(Msg::Eof) | Err(RecvTimeoutError::Disconnected) => (scanner.finish(), true),
            Err(RecvTimeoutError::Timeout) => continue,
            Ok(Msg::Code(r)) => {
                code_pending = false;
                match r {
                    Ok(line) if !prompted => {
                        early = Some(line);
                        continue;
                    }
                    Ok(line) => match relay(&master, &line) {
                        Ok(()) => continue,
                        Err(e) => break Err(("refused", format!("cannot send the code to claude: {e}"))),
                    },
                    Err(InputError::Aborted | InputError::Empty) => {
                        break Err(("cancelled", "no code was entered".to_owned()))
                    }
                    Err(e) => break Err(("refused", e.message().to_owned())),
                }
            }
        };
        let mut token = None;
        for ev in events {
            match ev {
                Event::Url(u) => out.url(&u),
                Event::PromptReady => {
                    out.code_needed();
                    prompted = true;
                    if let Some(line) = early.take() {
                        if let Err(e) = relay(&master, &line) {
                            return Err(("refused", format!("cannot send the code to claude: {e}")));
                        }
                    } else if !out.json {
                        code_pending = true;
                        ask_code(out, tx.clone());
                    }
                }
                Event::Token(t) => token = Some(t),
            }
        }
        if let Some(t) = token {
            break Ok(t);
        }
        if eof {
            break Err((
                "no_token",
                "claude exited without printing a token (wrong code, or the sign-in was refused)".to_owned(),
            ));
        }
    };
    if code_pending && !out.json {
        // The code prompt is still blocked in its thread, with echo off.
        tty::restore_echo();
    }
    result
}

/// Reads the code in a thread, so the timeout and signals still work.
fn ask_code(out: &Out, tx: Sender<Msg>) {
    let piped = !tty::stdin_is_tty();
    let prompt = if out.json {
        ""
    } else {
        "Paste the code from the browser: "
    };
    std::thread::spawn(move || {
        let _ = tx.send(Msg::Code(tty::read_line(prompt, piped)));
    });
}

/// The code, then Enter.
fn relay(master: &OwnedFd, line: &Line) -> Result<(), Errno> {
    let code = line.as_bytes().trim_ascii();
    write_all(master, code)?;
    std::thread::sleep(ENTER_DELAY);
    write_all(master, b"\r")
}
