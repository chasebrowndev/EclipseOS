// SPDX-License-Identifier: AGPL-3.0-only
//! Remote cover art, fetched by spawning `curl` (ADR 0065, "Remote album
//! art"). The services link no HTTP or TLS client; `curl` is argv-exec'd with
//! no shell, `https` only, no `~/.curlrc`, cookies or credentials, stderr on
//! `/dev/null`, in its own process group with `PR_SET_PDEATHSIG(SIGKILL)`, and
//! killed at a wall-clock deadline or the moment its output passes the size
//! cap.
//!
//! The URL is never on argv, where any process could read it from
//! `/proc/<pid>/cmdline`: it goes to curl on stdin as a one-line config
//! (`-K -`) holding a single quoted, escaped `url` (see [`config`]).
//!
//! The URL and the body are the human's media: nothing here logs them.

use std::io::{self, Read, Write};
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

/// Longest URL handed to curl. Keeps the config well under a pipe buffer.
const URL_MAX: usize = 8 * 1024;

/// Why a fetch gave no art. Only the word is ever logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    NotHttps,
    /// Control characters, or over [`URL_MAX`]: never handed to curl.
    BadUrl,
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
            Failure::BadUrl => "unusable url",
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
/// service thread. Anything but `https://`, or a URL [`config`] refuses, is
/// refused before a process starts.
pub(super) fn fetch(url: &str) -> Result<Arc<[u8]>, Failure> {
    if !url.starts_with("https://") {
        return Err(Failure::NotHttps);
    }
    let config = config(url).ok_or(Failure::BadUrl)?;
    run_capped(&argv(HTTPS_ONLY), config.as_bytes(), ART_MAX_BYTES, DEADLINE)
}

/// The config curl reads on stdin: one `url = "…"` line. Inside curl's double
/// quotes only `\` and `"` need escaping to keep the value from ending early;
/// a line break would start a new option, so any control character (C0, DEL,
/// C1) refuses the URL outright, as does one over [`URL_MAX`]. `None`: do not
/// fetch.
fn config(url: &str) -> Option<String> {
    if url.len() > URL_MAX || url.chars().any(char::is_control) {
        return None;
    }
    let mut line = String::with_capacity(url.len() + 16);
    line.push_str("url = \"");
    for c in url.chars() {
        if matches!(c, '\\' | '"') {
            line.push('\\');
        }
        line.push(c);
    }
    line.push_str("\"\n");
    Some(line)
}

/// curl's argv; the URL comes on stdin. `-q` must come first to take effect:
/// it skips `~/.curlrc`. `-K -` reads only the [`config`] written to stdin.
/// No `-b`/`-c` (cookies) or `-n` (netrc), so none are used.
fn argv(proto: &str) -> Vec<String> {
    [
        "curl",
        "-q",
        "-K",
        "-",
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
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// Run `argv` with `input` on its stdin, collect at most `cap` bytes of
/// stdout, and require a clean exit within `deadline`. Over the cap or past
/// the deadline, the process group is killed and reaped.
fn run_capped(argv: &[String], input: &[u8], cap: u64, deadline: Duration) -> Result<Arc<[u8]>, Failure> {
    let until = Instant::now() + deadline;
    let mut child = spawn(argv).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => Failure::NoCurl,
        _ => Failure::Spawn,
    })?;
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        kill_and_reap(&mut child);
        return Err(Failure::Spawn);
    };
    // Blocking I/O cannot time out, so it runs on its own thread and this one
    // keeps the clock. After a kill the pipes close and the thread ends. curl
    // reads its whole config before it writes, so writing stdin first cannot
    // deadlock; a child that never reads it only makes the write fail.
    let input = input.to_vec();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("eclipse-media-curl".into())
        .spawn(move || {
            let _ = stdin.write_all(&input);
            drop(stdin);
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
        .stdin(Stdio::piped())
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
    fn argv_is_https_only_with_rc_skipped_first_and_no_url() {
        let a = argv(HTTPS_ONLY);
        assert_eq!(a[1], "-q", "-q only works as the first option");
        let at = |flag: &str| a.iter().position(|s| s == flag).map(|i| a[i + 1].as_str());
        assert_eq!(at("-K"), Some("-"), "config from stdin only");
        assert_eq!(at("--proto"), Some("=https"));
        assert_eq!(at("--proto-redir"), Some("=https"));
        assert_eq!(at("--max-time"), Some("5"));
        assert_eq!(at("--max-filesize"), Some("4194304"));
        for banned in ["-b", "-c", "-n", "--netrc", "--cookie", "--", "--url"] {
            assert!(!a.iter().any(|s| s == banned), "{banned}");
        }
        assert!(!a.iter().any(|s| s.contains("://")), "the URL is never on argv");
    }

    #[test]
    fn config_quotes_and_escapes_the_url() {
        assert_eq!(
            config("https://x/a.png").as_deref(),
            Some("url = \"https://x/a.png\"\n")
        );
        // A quote cannot end the value early; a backslash cannot escape ours.
        assert_eq!(
            config(r#"https://x/a" -o "/tmp/pwn"#).as_deref(),
            Some("url = \"https://x/a\\\" -o \\\"/tmp/pwn\"\n")
        );
        assert_eq!(
            config(r"https://x/a\").as_deref(),
            Some("url = \"https://x/a\\\\\"\n")
        );
        assert_eq!(
            config("https://x/\u{e9}.png").as_deref(),
            Some("url = \"https://x/\u{e9}.png\"\n")
        );
    }

    /// Every quote in the value is escaped, so the only unescaped quotes are
    /// the two delimiters, and the config stays one line.
    #[test]
    fn config_never_ends_the_value_early() {
        for url in [
            r#"https://x/""""#,
            r#"https://x/\\"\"#,
            r#"https://x/\"#,
            "https://x/\"#",
        ] {
            let c = config(url).unwrap();
            assert_eq!(c.matches('\n').count(), 1, "{c}");
            let body = &c["url = \"".len()..c.len() - 2];
            let mut chars = body.chars();
            let mut unescaped = String::new();
            while let Some(ch) = chars.next() {
                match ch {
                    '\\' => unescaped.push(chars.next().expect("dangling backslash")),
                    '"' => panic!("unescaped quote in {c}"),
                    ch => unescaped.push(ch),
                }
            }
            assert_eq!(unescaped, url);
        }
    }

    #[test]
    fn control_characters_and_huge_urls_are_refused() {
        for url in [
            "https://x/a\n-o /tmp/pwn",
            "https://x/a\r\noutput = /tmp/pwn",
            "https://x/a\0",
            "https://x/a\t",
            "https://x/a\u{7f}",
            "https://x/a\u{85}",
        ] {
            assert_eq!(config(url), None, "{url:?}");
            assert_eq!(fetch(url), Err(Failure::BadUrl), "{url:?}");
        }
        let long = format!("https://x/{}", "a".repeat(URL_MAX));
        assert_eq!(fetch(&long), Err(Failure::BadUrl));
    }

    #[test]
    fn output_is_collected_on_clean_exit() {
        let got = run_capped(&sh("printf cover"), b"", 64, Duration::from_secs(5));
        assert_eq!(got.as_deref(), Ok(&b"cover"[..]));
    }

    #[test]
    fn input_reaches_stdin() {
        let got = run_capped(&["cat".into()], b"url = \"x\"\n", 64, Duration::from_secs(5));
        assert_eq!(got.as_deref(), Ok(&b"url = \"x\"\n"[..]));
        // A child that never reads stdin is not blocked on, nor broken by it.
        let got = run_capped(
            &sh("printf cover"),
            &[b'x'; 256 * 1024],
            64,
            Duration::from_secs(5),
        );
        assert_eq!(got.as_deref(), Ok(&b"cover"[..]));
    }

    #[test]
    fn failures_are_classified() {
        let d = Duration::from_secs(5);
        assert_eq!(
            run_capped(&["/nonexistent/curl".into()], b"", 64, d),
            Err(Failure::NoCurl)
        );
        assert_eq!(
            run_capped(&sh("printf x; exit 22"), b"", 64, d),
            Err(Failure::Exit)
        );
        assert_eq!(run_capped(&sh("true"), b"", 64, d), Err(Failure::Empty));
    }

    #[test]
    fn the_size_cap_kills_the_process() {
        let started = Instant::now();
        // Would stream forever; the cap stops it.
        let got = run_capped(
            &["cat".into(), "/dev/zero".into()],
            b"",
            1024,
            Duration::from_secs(5),
        );
        assert_eq!(got, Err(Failure::TooBig));
        assert!(started.elapsed() < Duration::from_secs(2));
        // Exactly at the cap is fine.
        let got = run_capped(&sh("head -c 1024 /dev/zero"), b"", 1024, Duration::from_secs(5));
        assert_eq!(got.map(|b| b.len()), Ok(1024));
    }

    #[test]
    fn the_deadline_kills_the_whole_group() {
        let started = Instant::now();
        // A background job holding stdout open: only a group kill ends it.
        let got = run_capped(&sh("sleep 30 & sleep 30"), b"", 64, Duration::from_millis(200));
        assert_eq!(got, Err(Failure::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Real curl, end to end, against a local `file://` URL: the spawn, the
    /// read, the stdin config and the cap. Ignored because it needs `curl` on
    /// `PATH`; never touches the network.
    #[test]
    #[ignore = "needs curl on PATH"]
    fn real_curl_reads_a_local_file_and_enforces_the_cap() {
        let dir = std::env::temp_dir().join(format!("eclipse-media-curl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let get = |url: &str, proto: &str, cap: u64| {
            run_capped(&argv(proto), config(url).unwrap().as_bytes(), cap, DEADLINE)
        };
        let small = dir.join("cover.png");
        std::fs::write(&small, b"\x89PNG-cover").unwrap();
        let url = format!("file://{}", small.display());
        assert_eq!(
            get(&url, "=file", ART_MAX_BYTES).as_deref(),
            Ok(&b"\x89PNG-cover"[..])
        );

        // With https-only, curl itself refuses the same URL.
        assert_eq!(get(&url, HTTPS_ONLY, ART_MAX_BYTES), Err(Failure::Exit));

        // Quotes and backslashes reach curl as part of the URL.
        let odd = dir.join(r#"co"v\er.png"#);
        std::fs::write(&odd, b"odd").unwrap();
        let url = format!("file://{}", odd.display());
        assert_eq!(get(&url, "=file", ART_MAX_BYTES).as_deref(), Ok(&b"odd"[..]));

        // An attempt to close the quote and add an option stays in the URL:
        // no such file, and nothing written.
        let pwn = dir.join("pwn");
        let url = format!(r#"file://{}" -o "{}"#, small.display(), pwn.display());
        assert_eq!(get(&url, "=file", ART_MAX_BYTES), Err(Failure::Exit));
        let url = format!(r#"file://{}\" output = "{}"#, small.display(), pwn.display());
        assert_eq!(get(&url, "=file", ART_MAX_BYTES), Err(Failure::Exit));
        assert!(!pwn.exists());

        let big = dir.join("big.png");
        std::fs::write(&big, vec![7u8; 64 * 1024]).unwrap();
        let url = format!("file://{}", big.display());
        assert_eq!(get(&url, "=file", 1024), Err(Failure::TooBig));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
