// SPDX-License-Identifier: AGPL-3.0-only
//! Real-pty tests: children are spawned via argv arrays (`/bin/sh -c ...`).

use std::ffi::OsString;
use std::time::{Duration, Instant};

use apoth_pty::{CloseTimings, Enqueue, EnvSpec, ExitStatus, Pty, PtySize, SpawnConfig, build_env};
use rustix::event::Timespec;
use rustix::event::{PollFd, PollFlags, poll};

fn os(s: &str) -> OsString {
    OsString::from(s)
}

fn size() -> PtySize {
    PtySize::from_cells(24, 80, 8, 16)
}

fn cfg_env(script: &str, env: Vec<(OsString, OsString)>) -> SpawnConfig {
    SpawnConfig {
        argv: vec![os("/bin/sh"), os("-c"), os(script)],
        login: false,
        cwd: None,
        env,
        size: size(),
        iutf8: true,
    }
}

fn default_env() -> Vec<(OsString, OsString)> {
    build_env(
        std::env::vars_os(),
        &EnvSpec {
            term: "xterm-256color",
            version: "0.0.0",
            extra: vec![],
        },
    )
}

fn cfg(script: &str) -> SpawnConfig {
    cfg_env(script, default_env())
}

fn wait_readable(p: &Pty, ms: i64) {
    let ts = Timespec {
        tv_sec: 0,
        tv_nsec: ms * 1_000_000,
    };
    let mfd = p.master_fd();
    let mut fds = [PollFd::new(&mfd, PollFlags::IN)];
    let _ = poll(&mut fds, Some(&ts));
}

/// Read until EIO/EOF or timeout.
fn read_all(p: &Pty, within: Duration) -> Vec<u8> {
    let end = Instant::now() + within;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while Instant::now() < end {
        match p.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => wait_readable(p, 50),
            Err(_) => break, // EIO: slave closed
        }
    }
    out
}

fn wait_exit(p: &mut Pty) -> ExitStatus {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(s) = p.try_wait().unwrap() {
            return s;
        }
        assert!(Instant::now() < end, "child did not exit");
        let ts = Timespec {
            tv_sec: 0,
            tv_nsec: 50_000_000,
        };
        let pfd = p.pidfd();
        let mut fds = [PollFd::new(&pfd, PollFlags::IN)];
        let _ = poll(&mut fds, Some(&ts));
    }
}

fn quick() -> CloseTimings {
    CloseTimings {
        hup_grace: Duration::from_millis(100),
        term_grace: Duration::from_millis(100),
        kill_reap: Duration::from_secs(2),
    }
}

#[test]
fn printf_output_and_exit_code() {
    let mut p = Pty::spawn(&cfg("printf 'hello'; exit 7")).unwrap();
    let out = read_all(&p, Duration::from_secs(5));
    assert!(String::from_utf8_lossy(&out).contains("hello"));
    assert_eq!(wait_exit(&mut p), ExitStatus::Code(7));
}

#[test]
fn killed_by_signal_is_reported() {
    let mut p = Pty::spawn(&cfg("kill -9 $$")).unwrap();
    assert_eq!(wait_exit(&mut p), ExitStatus::Signal(9));
}

#[test]
fn has_controlling_tty_and_own_session() {
    let mut p = Pty::spawn(&cfg("tty; exit 0")).unwrap();
    let out = String::from_utf8_lossy(&read_all(&p, Duration::from_secs(5))).into_owned();
    assert!(out.contains("/dev/pts/"), "tty said {out:?}");
    wait_exit(&mut p);
}

#[test]
fn input_reaches_child_and_echoes() {
    let mut p = Pty::spawn(&cfg("cat")).unwrap();
    assert_eq!(p.send(b"hi\n".to_vec()).unwrap(), Enqueue::Written);
    let out = read_all(&p, Duration::from_millis(500));
    assert!(String::from_utf8_lossy(&out).contains("hi"));
    p.close(&quick()).unwrap();
}

#[test]
fn argv_is_not_a_shell_string() {
    let c = SpawnConfig {
        argv: vec![os("/bin/echo"), os("a;b $HOME `x`")],
        ..cfg("")
    };
    let mut p = Pty::spawn(&c).unwrap();
    let out = String::from_utf8_lossy(&read_all(&p, Duration::from_secs(5))).into_owned();
    assert!(out.contains("a;b $HOME `x`"), "{out:?}");
    wait_exit(&mut p);
}

#[test]
fn resize_pushes_pixels_and_skips_noop() {
    let mut p = Pty::spawn(&cfg("sleep 5")).unwrap();
    assert_eq!(p.kernel_winsize().unwrap(), size());
    let s2 = PtySize::from_cells(30, 100, 8, 16);
    assert!(p.resize(s2).unwrap());
    assert_eq!(p.kernel_winsize().unwrap(), s2);
    assert_eq!((s2.xpixel, s2.ypixel), (800, 480));
    assert!(!p.resize(s2).unwrap());
    // pixel-only change (scale or cell size) still pushed
    let s3 = PtySize::from_cells(30, 100, 10, 20);
    assert!(p.resize(s3).unwrap());
    assert_eq!(p.kernel_winsize().unwrap(), s3);
    p.close(&quick()).unwrap();
}

#[test]
fn fd_hygiene_no_leaked_fds() {
    // fds above 2 in the child (bash may keep 255 for itself in some modes; the glob's own dir fd is gone by the -e recheck).
    let mut p = Pty::spawn(&cfg(
        "for f in /proc/$$/fd/*; do n=${f##*/}; if [ \"$n\" -gt 2 ] && [ \"$n\" -lt 255 ] && [ -e \"$f\" ]; then echo LEAK$n; fi; done; echo done",
    ))
    .unwrap();
    let out = String::from_utf8_lossy(&read_all(&p, Duration::from_secs(5))).into_owned();
    assert!(out.contains("done"), "{out:?}");
    assert!(!out.contains("LEAK"), "{out:?}");
    wait_exit(&mut p);
}

#[test]
fn environment_scrub_and_set() {
    let base = vec![
        (os("PATH"), os(&std::env::var("PATH").unwrap_or_default())),
        (os("KITTY_WINDOW_ID"), os("1")),
        (os("WEZTERM_PANE"), os("1")),
        (os("XDG_ACTIVATION_TOKEN"), os("secret")),
        (os("DESKTOP_STARTUP_ID"), os("secret")),
        (os("VTE_VERSION"), os("1")),
    ];
    let env = build_env(
        base,
        &EnvSpec {
            term: "xterm-256color",
            version: "9.9",
            extra: vec![(os("APOTHEOSIS_PANE"), os("42"))],
        },
    );
    let mut p = Pty::spawn(&cfg_env("env", env)).unwrap();
    let out = String::from_utf8_lossy(&read_all(&p, Duration::from_secs(5))).into_owned();
    for gone in [
        "KITTY_",
        "WEZTERM_",
        "XDG_ACTIVATION_TOKEN",
        "DESKTOP_STARTUP_ID",
        "VTE_VERSION",
    ] {
        assert!(!out.contains(gone), "{gone} leaked: {out:?}");
    }
    for want in [
        "TERM_PROGRAM=Apotheosis",
        "TERM_PROGRAM_VERSION=9.9",
        "COLORTERM=truecolor",
        "APOTHEOSIS_PANE=42",
    ] {
        assert!(out.contains(want), "{want} missing: {out:?}");
    }
    wait_exit(&mut p);
}

#[test]
fn cwd_and_foreground_group() {
    let c = SpawnConfig {
        cwd: Some("/tmp".into()),
        ..cfg("sleep 5")
    };
    let mut p = Pty::spawn(&c).unwrap();
    assert_eq!(p.foreground_pgid().unwrap(), p.pid());
    assert_eq!(
        p.foreground_cwd().unwrap().canonicalize().unwrap(),
        std::path::Path::new("/tmp").canonicalize().unwrap()
    );
    assert!(!p.foreground_is_remote(&[]));
    p.close(&quick()).unwrap();
}

fn wait_for<F: Fn(&Pty) -> bool>(p: &Pty, f: F) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if f(p) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn termios_credential_signal() {
    let mut p = Pty::spawn(&cfg("stty -echo; sleep 5")).unwrap();
    assert!(wait_for(&p, |p| p.termios_state().unwrap().is_credential()));
    p.close(&quick()).unwrap();

    // both off (full-screen app) is not a credential prompt
    let mut p = Pty::spawn(&cfg("stty -echo -icanon; sleep 5")).unwrap();
    assert!(wait_for(&p, |p| !p.termios_state().unwrap().icanon));
    assert!(!p.termios_state().unwrap().is_credential());
    p.close(&quick()).unwrap();

    // default shell-prompt state: echo on, icanon on
    let mut p = Pty::spawn(&cfg("sleep 5")).unwrap();
    let t = p.termios_state().unwrap();
    assert!(t.echo && t.icanon && !t.is_credential());
    p.close(&quick()).unwrap();
}

#[test]
fn outbound_queue_bound_and_drain() {
    // The child never reads, so the tty input buffer fills and the master write blocks.
    let mut p = Pty::spawn(&cfg("sleep 5")).unwrap();
    // T-00 §4.2: 1 MiB plus the paste in flight. A 2 MiB paste on an empty queue is the
    // unit in flight: accepted, partly written, and not counted against the bound.
    let paste = vec![b'a'; 2 << 20];
    assert_eq!(p.send_paste(paste).unwrap(), Enqueue::Queued);
    assert!(p.wants_pollout());
    assert!(p.pending_out_bytes() > 1 << 20);
    // Ordinary units behind it: exactly 1 MiB fits, one byte more is dropped.
    assert_eq!(p.send(vec![b'b'; 1 << 20]).unwrap(), Enqueue::Queued);
    assert_eq!(p.send(vec![b'c']).unwrap(), Enqueue::Dropped);
    assert_eq!(p.dropped_units(), 1);
    assert!(!p.drain_out().unwrap()); // still blocked, no spin, no error
    p.close(&quick()).unwrap();
    assert!(!p.wants_pollout());
}

#[test]
fn oversized_ordinary_unit_is_dropped_on_empty_queue() {
    let mut p = Pty::spawn(&cfg("sleep 5")).unwrap();
    assert_eq!(p.send(vec![b'a'; (1 << 20) + 1]).unwrap(), Enqueue::Dropped);
    assert_eq!(p.dropped_units(), 1);
    p.close(&quick()).unwrap();
}

#[test]
fn close_kills_descendants_and_follows_escalation_timing() {
    // The leader exits on HUP but a background child ignores HUP and TERM: close must still
    // take the group down (final SIGKILL while the leader is an unreaped zombie).
    let pidfile = std::env::temp_dir().join(format!("apoth-pty-bg-{}", std::process::id()));
    let script = format!(
        "(trap '' HUP TERM; exec sleep 300) & echo $! > {f}; wait",
        f = pidfile.display()
    );
    let mut p = Pty::spawn(&cfg(&script)).unwrap();
    let mut bg = None;
    for _ in 0..100 {
        if let Ok(s) = std::fs::read_to_string(&pidfile)
            && let Ok(n) = s.trim().parse::<i32>()
        {
            bg = Some(n);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_file(&pidfile);
    let bg = bg.expect("background pid captured");
    std::thread::sleep(Duration::from_millis(200));
    p.close(&quick()).unwrap();
    for _ in 0..100 {
        match std::fs::read_to_string(format!("/proc/{bg}/stat")) {
            Err(_) => return,
            Ok(st) if st.contains(") Z") => return, // zombie awaiting init: dead
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    panic!("descendant {bg} survived close");
}

#[test]
fn close_waits_the_grace_per_step_before_escalating() {
    let mut p = Pty::spawn(&cfg("trap '' HUP TERM; while :; do sleep 1; done")).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let t = CloseTimings {
        hup_grace: Duration::from_millis(300),
        term_grace: Duration::from_millis(300),
        kill_reap: Duration::from_secs(2),
    };
    let t0 = std::time::Instant::now();
    assert_eq!(p.close(&t).unwrap(), Some(ExitStatus::Signal(9)));
    assert!(
        t0.elapsed() >= Duration::from_millis(600),
        "{:?}",
        t0.elapsed()
    );
}

#[test]
fn no_group_signal_after_reap() {
    let mut p = Pty::spawn(&cfg("exit 0")).unwrap();
    assert_eq!(wait_exit(&mut p), ExitStatus::Code(0));
    // Reaped: signal_group is a no-op (the pgid may be reused) and close does not signal.
    p.signal_group(rustix::process::Signal::KILL).unwrap();
    assert_eq!(p.close(&quick()).unwrap(), Some(ExitStatus::Code(0)));
}

#[test]
fn from_cells_saturates_instead_of_overflowing() {
    let s = PtySize::from_cells(65535, 65535, u32::MAX, u32::MAX);
    assert_eq!((s.xpixel, s.ypixel), (u16::MAX, u16::MAX));
    let s = PtySize::from_cells(24, 80, 10, 20);
    assert_eq!((s.xpixel, s.ypixel), (800, 480));
}

#[test]
fn spawn_config_debug_shows_counts_only() {
    let c = cfg_env(
        "echo topsecret-cmd",
        vec![(os("TOKEN"), os("s3cr3t-value"))],
    );
    let d = format!("{c:?}");
    assert!(
        !d.contains("topsecret") && !d.contains("s3cr3t") && !d.contains("TOKEN"),
        "{d}"
    );
    assert!(d.contains("env_vars: 1"), "{d}");
}

#[test]
fn close_escalates_past_ignored_hup_and_term() {
    let mut p = Pty::spawn(&cfg("trap '' HUP TERM; while :; do sleep 1; done")).unwrap();
    std::thread::sleep(Duration::from_millis(200)); // let the trap install
    let st = p.close(&quick()).unwrap();
    assert_eq!(st, Some(ExitStatus::Signal(9)));
}

#[test]
fn empty_argv_rejected() {
    let c = SpawnConfig {
        argv: vec![],
        ..cfg("")
    };
    assert!(Pty::spawn(&c).is_err());
}
