// SPDX-License-Identifier: AGPL-3.0-only
//! Remote cover art, fetched by spawning `curl` (ADR 0065, "Remote album
//! art"). The services link no HTTP or TLS client; `curl` is argv-exec'd with
//! no shell, `https` only, no config file, cookies or credentials, stdin and
//! stderr on `/dev/null`, in its own process group with
//! `PR_SET_PDEATHSIG(SIGKILL)`, and killed at a wall-clock deadline or the
//! moment its output passes the size cap.
//!
//! The URL and the body are the human's media: nothing here logs them.

use std::io::{self, Read};
use std::os::raw::c_int;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::model::ART_MAX_BYTES;

// libc is linked by std on every unix target; no `libc` dependency for three
// calls. `custom::child` has the same shims but keeps them module-private.
extern "C" {
    fn prctl(option: c_int, ...) -> c_int;
    fn getppid() -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
}

const PR_SET_PDEATHSIG: c_int = 1;
const SIGKILL: c_int = 9;
const ESRCH: i32 = 3;

/// Our own deadline, over curl's `--max-time 5`: a curl that ignores its own
/// timeout (or hangs before starting one) is killed anyway.
const DEADLINE: Duration = Duration::from_secs(6);

/// The protocols curl may speak, for the request and for every redirect.
/// Only the ignored end-to-end test passes anything else (`=file`).
const HTTPS_ONLY: &str = "=https";

/// Why a fetch gave no art. Only the word is ever logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    NotHttps,
    NoCurl,
    Spawn,
    Exit,
    Timeout,
    TooBig,
    Empty,
}

impl Failure {
    pub fn word(self) -> &'static str {
        match self {
            Failure::NotHttps => "not https",
            Failure::NoCurl => "curl not found",
            Failure::Spawn => "cannot start curl",
            Failure::Exit => "fetch failed",
            Failure::Timeout => "fetch timed out",
            Failure::TooBig => "art over the size cap",
            Failure::Empty => "empty response",
        }
    }
}

/// Fetch an `https://` cover. Blocking (up to [`DEADLINE`]): call it off the
/// service thread. Anything but `https://` is refused before a process starts.
pub(super) fn fetch(url: &str) -> Result<Arc<[u8]>, Failure> {
    if !url.starts_with("https://") {
        return Err(Failure::NotHttps);
    }
    run_capped(&argv(url, HTTPS_ONLY), ART_MAX_BYTES, DEADLINE)
}

/// curl's argv. `-q` must come first to take effect: it skips `~/.curlrc`.
/// No `-b`/`-c` (cookies), `-K` or `-n` (netrc), so none are used.
fn argv(url: &str, proto: &str) -> Vec<String> {
    [
        "curl",
        "-q",
        "--proto",
        proto,
        "--proto-redir",
        proto,
        "--max-redirs",
        "3",
        "--max-time",
        "5",
        "--max-filesize",
        "4194304",
        "-fsS",
        "--no-progress-meter",
        "-o",
        "-",
        "--",
        url,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// Run `argv`, collect at most `cap` bytes of stdout, and require a clean
/// exit within `deadline`. Over the cap or past the deadline, the process
/// group is killed and reaped.
fn run_capped(argv: &[String], cap: u64, deadline: Duration) -> Result<Arc<[u8]>, Failure> {
    let until = Instant::now() + deadline;
    let mut child = spawn(argv).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => Failure::NoCurl,
        _ => Failure::Spawn,
    })?;
    let Some(mut stdout) = child.stdout.take() else {
        kill_and_reap(&mut child);
        return Err(Failure::Spawn);
    };
    // A blocking read cannot time out, so it runs on its own thread and this
    // one keeps the clock. After a kill the pipe closes and the reader ends.
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("eclipse-media-curl".into())
        .spawn(move || {
            let mut body = Vec::new();
            let read = (&mut stdout).take(cap + 1).read_to_end(&mut body);
            let _ = tx.send(read.map(|_| body));
        });
    if reader.is_err() {
        kill_and_reap(&mut child);
        return Err(Failure::Spawn);
    }
    let body = match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => {
            kill_and_reap(&mut child);
            return Err(Failure::Exit);
        }
        Err(_) => {
            kill_and_reap(&mut child);
            return Err(Failure::Timeout);
        }
    };
    if body.len() as u64 > cap {
        kill_and_reap(&mut child);
        return Err(Failure::TooBig);
    }
    // stdout is closed; curl exits right after. Still bounded by the deadline.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                kill_and_reap(&mut child);
                return Err(Failure::Timeout);
            }
        }
    };
    if !status.success() {
        return Err(Failure::Exit);
    }
    if body.is_empty() {
        return Err(Failure::Empty);
    }
    Ok(body.into())
}

fn spawn(argv: &[String]) -> io::Result<Child> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let parent = std::process::id() as c_int;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    // SAFETY: runs between fork and exec, calling only async-signal-safe
    // functions (prctl, getppid) and allocating nothing.
    unsafe {
        cmd.pre_exec(move || {
            if prctl(PR_SET_PDEATHSIG, SIGKILL as std::os::raw::c_ulong) != 0 {
                return Err(io::Error::last_os_error());
            }
            // The spawning thread may already be gone; then the signal never
            // comes, so refuse to run orphaned.
            if getppid() != parent {
                return Err(io::Error::from_raw_os_error(ESRCH));
            }
            Ok(())
        });
    }
    cmd.spawn()
}

/// SIGKILL `child`'s process group (it leads one; see [`spawn`]) and reap it.
/// Call only while `child` is unreaped, so its pid still names its group.
fn kill_and_reap(child: &mut Child) {
    if let Ok(pid) = c_int::try_from(child.id()) {
        // SAFETY: plain syscall wrapper; a gone group gives ESRCH.
        unsafe {
            kill(-pid, SIGKILL);
        }
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn only_https_is_fetched() {
        assert_eq!(fetch("http://example.com/a.png"), Err(Failure::NotHttps));
        assert_eq!(fetch("file:///etc/passwd"), Err(Failure::NotHttps));
        assert_eq!(fetch("HTTPS://example.com/a.png"), Err(Failure::NotHttps));
    }

    #[test]
    fn argv_is_https_only_with_rc_skipped_first() {
        let a = argv("https://x/a.png", HTTPS_ONLY);
        assert_eq!(a[1], "-q", "-q only works as the first option");
        let at = |flag: &str| a.iter().position(|s| s == flag).map(|i| a[i + 1].as_str());
        assert_eq!(at("--proto"), Some("=https"));
        assert_eq!(at("--proto-redir"), Some("=https"));
        assert_eq!(at("--max-filesize"), Some("4194304"));
        assert_eq!(a[a.len() - 2..], ["--".to_owned(), "https://x/a.png".to_owned()]);
        for banned in ["-b", "-c", "-K", "-n", "--netrc", "--cookie"] {
            assert!(!a.iter().any(|s| s == banned), "{banned}");
        }
    }

    #[test]
    fn output_is_collected_on_clean_exit() {
        let got = run_capped(&sh("printf cover"), 64, Duration::from_secs(5));
        assert_eq!(got.as_deref(), Ok(&b"cover"[..]));
    }

    #[test]
    fn failures_are_classified() {
        let d = Duration::from_secs(5);
        assert_eq!(
            run_capped(&["/nonexistent/curl".into()], 64, d),
            Err(Failure::NoCurl)
        );
        assert_eq!(run_capped(&sh("printf x; exit 22"), 64, d), Err(Failure::Exit));
        assert_eq!(run_capped(&sh("true"), 64, d), Err(Failure::Empty));
    }

    #[test]
    fn the_size_cap_kills_the_process() {
        let started = Instant::now();
        // Would stream forever; the cap stops it.
        let got = run_capped(&["cat".into(), "/dev/zero".into()], 1024, Duration::from_secs(5));
        assert_eq!(got, Err(Failure::TooBig));
        assert!(started.elapsed() < Duration::from_secs(2));
        // Exactly at the cap is fine.
        let got = run_capped(&sh("head -c 1024 /dev/zero"), 1024, Duration::from_secs(5));
        assert_eq!(got.map(|b| b.len()), Ok(1024));
    }

    #[test]
    fn the_deadline_kills_the_whole_group() {
        let started = Instant::now();
        // A background job holding stdout open: only a group kill ends it.
        let got = run_capped(&sh("sleep 30 & sleep 30"), 64, Duration::from_millis(200));
        assert_eq!(got, Err(Failure::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Real curl, end to end, against a local `file://` URL: the spawn, the
    /// read and the cap. Ignored because it needs `curl` on `PATH`; never
    /// touches the network.
    #[test]
    #[ignore = "needs curl on PATH"]
    fn real_curl_reads_a_local_file_and_enforces_the_cap() {
        let dir = std::env::temp_dir().join(format!("eclipse-media-curl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("cover.png");
        std::fs::write(&small, b"\x89PNG-cover").unwrap();
        let url = format!("file://{}", small.display());
        let got = run_capped(&argv(&url, "=file"), ART_MAX_BYTES, DEADLINE);
        assert_eq!(got.as_deref(), Ok(&b"\x89PNG-cover"[..]));

        // With https-only, curl itself refuses the same URL.
        assert_eq!(
            run_capped(&argv(&url, HTTPS_ONLY), ART_MAX_BYTES, DEADLINE),
            Err(Failure::Exit)
        );

        let big = dir.join("big.png");
        std::fs::write(&big, vec![7u8; 64 * 1024]).unwrap();
        let url = format!("file://{}", big.display());
        assert_eq!(
            run_capped(&argv(&url, "=file"), 1024, DEADLINE),
            Err(Failure::TooBig)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
