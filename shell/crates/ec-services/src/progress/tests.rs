// SPDX-License-Identifier: AGPL-3.0-only
//! Service tests: the socket end to end, and the scan through a fake
//! `/proc`. Nothing here reads the live `/proc` or the session's socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ec_ipc::progress::MAX_LINE;
use serde_json::Value;

use super::scan::{IoStat, Proc, ProcSource};
use super::{spawn_with, Options, Source, Update, MAX_CLIENTS};

struct Fixed(Vec<Proc>);

impl ProcSource for Fixed {
    fn procs(&mut self) -> Vec<Proc> {
        self.0.clone()
    }
}

fn proc(pid: u32, comm: &str, words: &[&str]) -> Proc {
    Proc {
        pid,
        comm: comm.into(),
        argv: words.iter().map(|s| s.to_string()).collect(),
        ..Proc::default()
    }
}

fn temp_dir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ec-progress-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// One request, one reply, over a fresh connection.
fn ask(sock: &PathBuf, line: &str) -> Value {
    let mut s = UnixStream::connect(sock).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(format!("{line}\n").as_bytes()).unwrap();
    let mut buf = String::new();
    BufReader::new(s).read_line(&mut buf).unwrap();
    serde_json::from_str(&buf).expect("json reply")
}

/// Wait until `pred` holds for the newest list the service sent.
fn await_list(h: &super::Handle, pred: impl Fn(&[super::Job]) -> bool) -> Vec<super::Job> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = Vec::new();
    loop {
        while let Some(Update::Progress(jobs)) = h.try_recv() {
            last = jobs;
        }
        if pred(&last) {
            return last;
        }
        assert!(Instant::now() < deadline, "no matching list; last {last:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn feed_round_trip_over_the_socket() {
    let dir = temp_dir();
    let sock = dir.join("progress.sock");
    let h = spawn_with(Options {
        socket: Some(sock.clone()),
        source: Box::new(Fixed(vec![])),
    });

    assert_eq!(
        std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777,
        0o600,
        "socket is user-only"
    );
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700,
        "directory is user-only"
    );

    let r = ask(
        &sock,
        r#"{"op":"start","id":"build","label":"cargo build","total":100}"#,
    );
    assert_eq!(r["ok"], true);
    let r = ask(&sock, r#"{"op":"set","id":"build","bytes":25}"#);
    assert_eq!(r["ok"], true);

    let jobs = await_list(&h, |j| {
        j.iter().any(|j| j.id == "build" && j.fraction == Some(0.25))
    });
    assert_eq!(jobs[0].source, Source::Feed);
    assert_eq!(jobs[0].label, "cargo build");

    let listed = ask(&sock, r#"{"op":"list"}"#);
    assert_eq!(listed["jobs"][0]["id"], "build");
    assert_eq!(listed["jobs"][0]["fraction"], serde_json::json!(0.25));

    assert_eq!(ask(&sock, r#"{"op":"done","id":"build"}"#)["ok"], true);
    await_list(&h, |j| j.is_empty());
    assert_eq!(ask(&sock, r#"{"op":"done","id":"build"}"#)["ok"], false);

    drop(h);
    let deadline = Instant::now() + Duration::from_secs(5);
    while sock.exists() {
        assert!(Instant::now() < deadline, "socket outlives its handle");
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refused_requests_are_answered_and_the_connection_survives() {
    let dir = temp_dir();
    let sock = dir.join("progress.sock");
    let _h = spawn_with(Options {
        socket: Some(sock.clone()),
        source: Box::new(Fixed(vec![])),
    });

    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut reader = BufReader::new(s.try_clone().unwrap());
    for bad in ["garbage", r#"{"op":"set","id":"x","fraction":3}"#] {
        s.write_all(format!("{bad}\n").as_bytes()).unwrap();
        let mut buf = String::new();
        reader.read_line(&mut buf).unwrap();
        let v: Value = serde_json::from_str(&buf).unwrap();
        assert_eq!(v["ok"], false, "{bad}");
        assert!(v["error"].is_string());
    }
    // Same connection still answers a good request.
    s.write_all(b"{\"op\":\"list\"}\n").unwrap();
    let mut buf = String::new();
    reader.read_line(&mut buf).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&buf).unwrap()["ok"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oversized_line_drops_the_client() {
    let dir = temp_dir();
    let sock = dir.join("progress.sock");
    let _h = spawn_with(Options {
        socket: Some(sock.clone()),
        source: Box::new(Fixed(vec![])),
    });
    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let junk = vec![b'a'; ec_ipc::progress::MAX_LINE + 10];
    let _ = s.write_all(&junk);
    let mut out = String::new();
    let _ = BufReader::new(s).read_line(&mut out);
    assert!(out.contains("too long") || out.is_empty(), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn serve_here() -> (super::Handle, PathBuf, PathBuf) {
    let dir = temp_dir();
    let sock = dir.join("progress.sock");
    let h = spawn_with(Options {
        socket: Some(sock.clone()),
        source: Box::new(Fixed(vec![])),
    });
    (h, dir, sock)
}

#[test]
fn request_then_half_close_is_answered_before_the_drop() {
    let (_h, dir, sock) = serve_here();
    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"{\"op\":\"list\"}\n").unwrap();
    s.shutdown(std::net::Shutdown::Write).unwrap();
    let mut buf = String::new();
    BufReader::new(s).read_line(&mut buf).unwrap();
    let v: Value = serde_json::from_str(&buf).expect("reply before close");
    assert_eq!(v["ok"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_line_past_the_cap_is_refused_with_or_without_a_newline() {
    let (_h, dir, sock) = serve_here();
    // Full buffer with no newline, then a complete line one byte too long.
    let unterminated = vec![b'a'; MAX_LINE + 1];
    let mut terminated = vec![b'a'; MAX_LINE + 1];
    terminated.push(b'\n');
    for junk in [unterminated, terminated] {
        let mut s = UnixStream::connect(&sock).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = s.write_all(&junk);
        let mut out = String::new();
        let _ = BufReader::new(s).read_line(&mut out);
        assert!(out.contains("too long"), "{out}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_line_at_the_limit_is_served() {
    let (_h, dir, sock) = serve_here();
    // Body of exactly MAX_LINE bytes: a valid request padded with spaces.
    let pad = " ".repeat(MAX_LINE - r#"{"op":"list"}"#.len());
    let line = format!("{}{pad}\n", r#"{"op":"list"}"#);
    assert_eq!(line.len(), MAX_LINE + 1);
    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(line.as_bytes()).unwrap();
    let mut buf = String::new();
    BufReader::new(s).read_line(&mut buf).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&buf).unwrap()["ok"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clients_past_the_cap_are_closed_on_accept() {
    let (_h, dir, sock) = serve_here();
    let mut held: Vec<UnixStream> = (0..MAX_CLIENTS)
        .map(|_| UnixStream::connect(&sock).unwrap())
        .collect();
    // The backlog is first in, first accepted, so the extra one arrives last.
    let extra = UnixStream::connect(&sock).unwrap();
    extra.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut out = String::new();
    let _ = BufReader::new(extra).read_line(&mut out);
    assert!(out.is_empty(), "extra client was served: {out}");

    // The held clients still get answers.
    let first = &mut held[0];
    first.write_all(b"{\"op\":\"list\"}\n").unwrap();
    let mut buf = String::new();
    BufReader::new(first.try_clone().unwrap())
        .read_line(&mut buf)
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&buf).unwrap()["ok"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_jobs_appear_and_stop_when_scanning_is_off() {
    let fake = vec![
        Proc {
            io: Some(IoStat { rchar: 0, wchar: 50 }),
            source_bytes: Some(200),
            ..proc(42, "cp", &["cp", "/data/disk.img", "/mnt/usb/"])
        },
        proc(43, "bash", &["bash"]),
    ];
    let h = spawn_with(Options {
        socket: None,
        source: Box::new(Fixed(fake)),
    });
    let jobs = await_list(&h, |j| !j.is_empty());
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].source, Source::Scan);
    assert_eq!(jobs[0].id, "pid:42");
    assert_eq!(jobs[0].label, "cp \u{2192} usb");
    assert_eq!(jobs[0].fraction, Some(0.25));

    h.set_scanning(false);
    await_list(&h, |j| j.is_empty());
}

#[test]
fn scan_and_feed_report_the_same_work_once() {
    let dir = temp_dir();
    let sock = dir.join("progress.sock");
    let fake = vec![proc(7, "cargo", &["cargo", "build"])];
    let h = spawn_with(Options {
        socket: Some(sock.clone()),
        source: Box::new(Fixed(fake)),
    });
    await_list(&h, |j| j.len() == 1 && j[0].source == Source::Scan);

    assert_eq!(
        ask(&sock, r#"{"op":"start","id":"b","label":"cargo build"}"#)["ok"],
        true
    );
    let jobs = await_list(&h, |j| j.len() == 1 && j[0].source == Source::Feed);
    assert_eq!(jobs[0].label, "cargo build");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_socket_that_cannot_bind_leaves_scan_working() {
    // The parent is a regular file, so the directory cannot be created.
    let dir = temp_dir();
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    std::fs::write(&dir, b"not a dir").unwrap();
    let h = spawn_with(Options {
        socket: Some(dir.join("progress.sock")),
        source: Box::new(Fixed(vec![proc(1, "tar", &["tar", "-t"])])),
    });
    let jobs = await_list(&h, |j| !j.is_empty());
    assert_eq!(jobs[0].label, "tar");
    let _ = std::fs::remove_file(&dir);
}
