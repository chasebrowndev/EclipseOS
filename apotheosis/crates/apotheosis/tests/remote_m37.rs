// SPDX-License-Identifier: AGPL-3.0-only
//! M37 gates: remote domains (T-22 §3–§5) and the tmux-model session kill (T-21 §3).
//!
//! No sshd is started or configured: a fake `ssh` (a shell script first on `PATH`) skips
//! the options, answers `-O check` with "no master", answers the `command -v apoth` probe
//! (present, or absent when `no-apoth` exists) and runs the remote command here, so the
//! remote end is this host's `apoth proxy` and session daemon. The wire is tested over a
//! socket pair given to `apoth proxy` as stdio; the window tests run `apotheosis --domain`
//! on a hidden headless compositor.
//!
//! `apoth` (the CLI) is a sibling binary of this crate's; without it the tests skip. The
//! daemon pre-grants it while it runs `proxy` (T-21 §3), so no test hook is set here.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use apoth_proto::mux::{Decoded, MUX_MAJOR, MUX_MINOR, RecordDecoder, ServerMsg, ViewMsg};
use apoth_term::{Term, TermConfig, TermImage};

/// The `apoth` CLI built next to `apotheosis`.
fn apoth_bin() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_BIN_EXE_apotheosis"))
        .parent()?
        .join("apoth");
    if p.is_file() {
        return std::fs::canonicalize(p).ok();
    }
    assert!(
        std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
        "the apoth binary is not built next to apotheosis"
    );
    eprintln!("skipping: the apoth binary is not built");
    None
}

/// Kills a child by the pid captured at spawn.
struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Server {
    child: Child,
    dir: PathBuf,
    sock: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("apm37{name}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home/.config/eclipse")).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn base_env(c: &mut Command, dir: &Path) {
    c.env("XDG_RUNTIME_DIR", dir)
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("home/.config"))
        .env("XDG_CACHE_HOME", dir.join("home/.cache"))
        .env("XDG_STATE_HOME", dir.join("home/.state"))
        .env("SHELL", "/bin/sh")
        .env("ENV", "/dev/null")
        .env_remove("APOTHEOSIS_SOCKET")
        .env_remove("APOTHEOSIS_ASKPASS_SOCK")
        .env_remove("APOTHEOSIS_ASKPASS_TOKEN")
        .env_remove("APOTH_TEST_ATTACH_EXE");
}

/// A session daemon `t`.
fn start_server(dir: &Path) -> Server {
    let mut c = Command::new(env!("CARGO_BIN_EXE_apotheosis"));
    base_env(&mut c, dir);
    c.env_remove("WAYLAND_DISPLAY")
        .args(["--server", "--session", "t"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = c.spawn().expect("spawn the server");
    let sock = dir.join("apotheosis/server-t.sock");
    let t0 = Instant::now();
    while UnixStream::connect(&sock).is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "the server never listened"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Server {
        child,
        dir: dir.to_owned(),
        sock,
    }
}

/// A snapshot being received: (sequence, state, pages per screen).
type Building = (u64, Vec<u8>, [Vec<Vec<u8>>; 2]);

#[derive(Default)]
struct PaneView {
    term: Option<Term>,
    next: u64,
    building: Option<Building>,
}

/// A protocol view over any stream (here: the far end of `apoth proxy`'s stdio).
struct View {
    s: UnixStream,
    dec: RecordDecoder,
    panes: BTreeMap<u64, PaneView>,
    refused: Option<String>,
    eof: bool,
}

impl View {
    fn new(s: UnixStream) -> View {
        s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        View {
            s,
            dec: RecordDecoder::new(),
            panes: BTreeMap::new(),
            refused: None,
            eof: false,
        }
    }

    fn send(&mut self, m: &ViewMsg) {
        let _ = self.s.write_all(&m.encode());
    }

    fn hello(&mut self) -> Option<ServerMsg> {
        self.send(&ViewMsg::Hello {
            major: MUX_MAJOR,
            minor: MUX_MINOR,
            caps: Vec::new(),
            client: "test".into(),
        });
        let t0 = Instant::now();
        loop {
            if let Some(m) = self.next_msg() {
                return Some(m);
            }
            if self.eof || t0.elapsed() > Duration::from_secs(10) {
                return None;
            }
        }
    }

    fn next_msg(&mut self) -> Option<ServerMsg> {
        loop {
            if let Some(r) = self.dec.next_record() {
                let rec = r.expect("well-formed record").to_vec();
                match ServerMsg::decode(&rec).expect("decodes") {
                    Decoded::Msg(m) => return Some(m),
                    Decoded::Skipped(_) => continue,
                }
            }
            let mut b = [0u8; 65536];
            match self.s.read(&mut b) {
                Ok(0) => {
                    self.eof = true;
                    return None;
                }
                Ok(n) => self.dec.feed(&b[..n]),
                Err(_) => return None,
            }
        }
    }

    fn apply(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::Refused { reason } => self.refused = Some(reason),
            ServerMsg::SnapBegin {
                pane, seq, state, ..
            } => {
                self.panes.entry(pane).or_default().building =
                    Some((seq, state, [Vec::new(), Vec::new()]));
            }
            ServerMsg::SnapPage {
                pane,
                screen,
                bytes,
            } => {
                let p = self.panes.get_mut(&pane).expect("page of a known pane");
                p.building.as_mut().expect("page inside a snapshot").2[usize::from(screen)]
                    .push(bytes);
            }
            ServerMsg::SnapEnd { pane } => {
                let p = self.panes.get_mut(&pane).unwrap();
                let (seq, state, pages) = p.building.take().expect("end of a snapshot");
                p.term = Some(
                    Term::restore(
                        TermConfig::default(),
                        TermImage { state, pages },
                        Instant::now(),
                    )
                    .expect("snapshot restores"),
                );
                p.next = seq;
            }
            ServerMsg::Tee { pane, seq, bytes } => {
                let p = self.panes.get_mut(&pane).expect("tee of a known pane");
                assert_eq!(seq, p.next, "tee sequence gap on pane {pane}");
                p.next += 1;
                let t = p.term.as_mut().expect("tee after the snapshot");
                t.feed(&bytes, Instant::now());
                while t.replies().pop().is_some() {}
                t.events().clear();
            }
            ServerMsg::Resized {
                pane,
                seq,
                rows,
                cols,
            } => {
                let p = self.panes.get_mut(&pane).unwrap();
                assert_eq!(seq, p.next, "resize sequence gap on pane {pane}");
                p.next += 1;
                p.term
                    .as_mut()
                    .unwrap()
                    .resize(usize::from(rows), usize::from(cols));
            }
            _ => {}
        }
    }

    fn until(&mut self, secs: u64, what: &str, mut pred: impl FnMut(&View) -> bool) {
        let t0 = Instant::now();
        while !pred(self) {
            assert!(
                t0.elapsed() < Duration::from_secs(secs),
                "timed out waiting for {what}; screen:\n{}",
                self.screen(1)
            );
            if let Some(m) = self.next_msg() {
                self.apply(m);
            } else if self.eof {
                assert!(pred(self), "stream ended waiting for {what}");
                return;
            }
        }
    }

    fn screen(&self, pane: u64) -> String {
        let Some(t) = self.panes.get(&pane).and_then(|p| p.term.as_ref()) else {
            return String::new();
        };
        (0..t.rows())
            .map(|y| t.row_text(y).trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// `apoth proxy` with its stdin and stdout on one end of a socket pair; the other end.
fn proxy(apoth: &Path, dir: &Path, extra: &[&str]) -> (Guard, UnixStream) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let out = theirs.try_clone().unwrap();
    let mut c = Command::new(apoth);
    base_env(&mut c, dir);
    c.args(["proxy", "--session", "t"])
        .args(extra)
        .stdin(Stdio::from(OwnedFd::from(theirs)))
        .stdout(Stdio::from(OwnedFd::from(out)))
        .stderr(Stdio::null());
    (Guard(c.spawn().expect("spawn apoth proxy")), ours)
}

#[test]
fn the_mux_wire_crosses_apoth_proxy_stdio() {
    let Some(apoth) = apoth_bin() else { return };
    let dir = temp_dir("wire");
    let srv = start_server(&dir);
    let (_p, s) = proxy(&apoth, &dir, &["--no-start"]);
    let mut v = View::new(s);
    assert!(
        matches!(v.hello(), Some(ServerMsg::Welcome { .. })),
        "the daemon greets through the proxy"
    );
    v.send(&ViewMsg::Attach);
    v.until(10, "the snapshot through the proxy", |v| {
        v.panes.get(&1).is_some_and(|p| p.term.is_some())
    });
    v.send(&ViewMsg::Input {
        pane: 1,
        bytes: b"echo wire-$((6*7))\r".to_vec(),
    });
    v.until(10, "the echo through the proxy", |v| {
        v.screen(1).contains("wire-42")
    });
    // A view leaving through the proxy (stdin closed) is a detach: the pane stays.
    drop(v);
    std::thread::sleep(Duration::from_millis(300));
    let (_q, s) = proxy(&apoth, &dir, &["--no-start"]);
    let mut w = View::new(s);
    assert!(matches!(w.hello(), Some(ServerMsg::Welcome { .. })));
    w.send(&ViewMsg::Attach);
    w.until(10, "the pane after the first proxy left", |v| {
        v.screen(1).contains("wire-42")
    });
    drop(srv);
}

#[test]
fn apoth_proxy_is_refused_from_inside_a_server_pane_and_without_a_daemon() {
    let Some(apoth) = apoth_bin() else { return };
    let dir = temp_dir("refuse");
    // No daemon and --no-start: exit 4, nothing started.
    let mut c = Command::new(&apoth);
    base_env(&mut c, &dir);
    let st = c
        .args(["proxy", "--session", "t", "--no-start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(4));
    assert!(!dir.join("apotheosis/server-t.sock").exists());
    // The same `apoth proxy`, run by a shell in one of the daemon's panes: refused (T-21 §3
    // refuses pane descendants before any pre-grant).
    let srv = start_server(&dir);
    let (_p, s) = proxy(&apoth, &dir, &["--no-start"]);
    let mut v = View::new(s);
    assert!(matches!(v.hello(), Some(ServerMsg::Welcome { .. })));
    v.send(&ViewMsg::Attach);
    v.until(10, "the snapshot", |v| {
        v.panes.get(&1).is_some_and(|p| p.term.is_some())
    });
    let mut req = ViewMsg::Hello {
        major: MUX_MAJOR,
        minor: MUX_MINOR,
        caps: Vec::new(),
        client: "inner".into(),
    }
    .encode();
    req.extend(ViewMsg::Attach.encode());
    std::fs::write(dir.join("in.bin"), req).unwrap();
    let (inb, outb) = (dir.join("in.bin"), dir.join("out.bin"));
    v.send(&ViewMsg::Input {
        pane: 1,
        bytes: format!(
            "(cat {}; sleep 3) | {} proxy --session t --no-start > {}.part; mv {}.part {}\r",
            inb.display(),
            apoth.display(),
            outb.display(),
            outb.display(),
            outb.display()
        )
        .into_bytes(),
    });
    assert!(wait_file(&outb, 20), "the inner proxy never finished");
    let mut dec = RecordDecoder::new();
    dec.feed(&std::fs::read(&outb).unwrap());
    let mut msgs = Vec::new();
    while let Some(r) = dec.next_record() {
        if let Ok(Decoded::Msg(m)) = ServerMsg::decode(r.expect("well-formed record")) {
            msgs.push(m);
        }
    }
    assert!(
        msgs.iter().any(|m| matches!(m, ServerMsg::Refused { .. })),
        "attach from inside a pane is refused"
    );
    assert!(
        !msgs
            .iter()
            .any(|m| matches!(m, ServerMsg::SnapBegin { .. })),
        "no content reaches a pane's proxy"
    );
    drop(v);
    drop(srv);
}

#[test]
fn server_kill_closes_every_pane_and_ends_the_daemon() {
    let Some(apoth) = apoth_bin() else { return };
    let dir = temp_dir("kill");
    let mut srv = start_server(&dir);
    let run = |args: &[&str]| {
        let mut c = Command::new(&apoth);
        base_env(&mut c, &dir);
        c.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .code()
    };
    // A foreground job in the pane: the hang-up reaches it.
    let (_p, s) = proxy(&apoth, &dir, &["--no-start"]);
    let mut v = View::new(s);
    assert!(matches!(v.hello(), Some(ServerMsg::Welcome { .. })));
    v.send(&ViewMsg::Attach);
    v.until(10, "the snapshot", |v| {
        v.panes.get(&1).is_some_and(|p| p.term.is_some())
    });
    let pidfile = dir.join("bg.pid");
    v.send(&ViewMsg::Input {
        pane: 1,
        bytes: format!("sh -c 'echo $$ > {}; exec sleep 300'\r", pidfile.display()).into_bytes(),
    });
    assert!(wait_file(&pidfile, 10), "the job never started");
    std::thread::sleep(Duration::from_millis(100));
    let bg: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    assert_eq!(run(&["server", "kill", "nope"]), Some(4), "no such session");
    assert_eq!(run(&["server", "kill", "t"]), Some(0));
    let t0 = Instant::now();
    let st = loop {
        if let Ok(Some(st)) = srv.child.try_wait() {
            break st;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "the daemon stayed");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(st.success(), "{st}");
    assert!(!srv.sock.exists(), "the socket is removed");
    let t0 = Instant::now();
    while Path::new(&format!("/proc/{bg}")).exists()
        && std::fs::read_to_string(format!("/proc/{bg}/stat")).is_ok_and(|s| !s.contains(") Z "))
    {
        if t0.elapsed() > Duration::from_secs(5) {
            // Clean up our own leftover (captured pid) before failing.
            let _ = Command::new("kill").arg(bg.to_string()).status();
            panic!("the pane's job survived the kill");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(v);
}

#[test]
fn askpass_without_an_armed_channel_exits_3() {
    let Some(apoth) = apoth_bin() else { return };
    let dir = temp_dir("askpass");
    let ask = |env: &[(&str, &str)]| {
        let mut c = Command::new(&apoth);
        base_env(&mut c, &dir);
        for (k, v) in env {
            c.env(k, v);
        }
        c.args(["askpass", "Password: "])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .unwrap()
    };
    // No channel at all.
    let o = ask(&[]);
    assert_eq!(o.status.code(), Some(3));
    assert!(o.stdout.is_empty());
    // A socket that does not exist, and a malformed token.
    let gone = dir.join("apotheosis/askpass-0000.sock");
    let gone = gone.to_str().unwrap();
    let token = "ab".repeat(32);
    let o = ask(&[
        ("APOTHEOSIS_ASKPASS_SOCK", gone),
        ("APOTHEOSIS_ASKPASS_TOKEN", &token),
    ]);
    assert_eq!(o.status.code(), Some(3));
    assert!(o.stdout.is_empty());
    let o = ask(&[
        ("APOTHEOSIS_ASKPASS_SOCK", gone),
        ("APOTHEOSIS_ASKPASS_TOKEN", "short"),
    ]);
    assert_eq!(o.status.code(), Some(3));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the window on a domain, on a hidden headless compositor ----------------------------

fn find_abyss() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("APOTH_EC_ABYSS") {
        return Some(PathBuf::from(p));
    }
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("ec-abyss"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        assert!(
            std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
            "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
        );
        eprintln!("skipping: ec-abyss not found");
    }
    found
}

/// The fake `ssh`: answers the `apoth` probe (logging it to `probes`), marks a remote
/// `apoth proxy` in `proxied`, records the pid that runs the remote command (it `exec`s it),
/// asks askpass once when `askpass.want` exists, and runs the command here.
const SHIM: &str = r#"#!/bin/sh
d=$(dirname "$0")/..
while [ $# -gt 0 ]; do
  case "$1" in
    -O) exit 255 ;;
    -o) shift 2 ;;
    -T) shift ;;
    --) shift; break ;;
    -*) shift ;;
    *) break ;;
  esac
done
dest=$1; shift
case "$*" in
  *"command -v apoth"*) echo probe >> "$d/probes"; [ -e "$d/no-apoth" ] && exit 1; exit 0 ;;
  *"apoth proxy"*) [ -e "$d/no-apoth" ] && exit 127; : > "$d/proxied" ;;
esac
if [ -e "$d/askpass.want" ]; then
  rm -f "$d/askpass.want"
  : > "$d/askpass.asked"
  ans=$("$SSH_ASKPASS" "$dest's password: ") || { : > "$d/askpass.cancelled"; exit 4; }
  if [ "$ans" = "s3cret-M37" ]; then : > "$d/askpass.ok"; else : > "$d/askpass.bad"; exit 4; fi
fi
[ -n "$APOTHEOSIS_ASKPASS_TOKEN" ] && [ -S "$APOTHEOSIS_ASKPASS_SOCK" ] && : > "$d/askpass.armed"
echo $$ >> "$d/ssh.pids"
[ $# -eq 0 ] && exec sh -i
exec sh -c "$*"
"#;

/// `dir/bin` with the fake `ssh` and `apoth`; the `PATH` putting it first.
fn shim_path(dir: &Path, apoth: &Path) -> String {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("ssh"), SHIM).unwrap();
    std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(apoth, bin.join("apoth")).unwrap();
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn write_config(dir: &Path, kdl: &str) {
    std::fs::write(dir.join("home/.config/eclipse/apotheosis.kdl"), kdl).unwrap();
}

/// `me@box` already answered (no terminfo, no environment): `apoth ssh` asks nothing.
fn pre_consent(dir: &Path) {
    let st = dir.join("home/.state/apotheosis");
    std::fs::create_dir_all(&st).unwrap();
    std::fs::write(
        st.join("ssh-hosts.kdl"),
        "host \"me@box\" terminfo=#false env=#false\n",
    )
    .unwrap();
}

/// `apotheosis ARGS` with the shim on `PATH`, no display unless `wayland` is given.
fn gui(dir: &Path, path: &str, args: &[&str], wayland: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_apotheosis"));
    base_env(&mut c, dir);
    c.args(args)
        .env("PATH", path)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/apoth-test-bus",
        )
        .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
        .env("APOTH_TEST_DUMP", dir.join("dump.txt"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if wayland {
        c.env("WAYLAND_DISPLAY", "wayland-1");
    } else {
        c.env_remove("WAYLAND_DISPLAY");
    }
    c
}

/// A hidden headless compositor on `dir`, with scripted input allowed.
fn start_abyss(abyss: &Path, dir: &Path) -> Guard {
    std::fs::create_dir_all(dir.join("sock")).unwrap();
    std::fs::create_dir_all(dir.join("cfg/eclipse")).unwrap();
    std::fs::write(
        dir.join("cfg/eclipse/policy.kdl"),
        "misc { scripted-input #true }\n",
    )
    .unwrap();
    let g = Guard(
        Command::new(abyss)
            .args(["--backend", "headless", "--size", "800x600"])
            .env("XDG_RUNTIME_DIR", dir)
            .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
            .env("XDG_CONFIG_HOME", dir.join("cfg"))
            .env_remove("WAYLAND_DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ec-abyss"),
    );
    let t0 = Instant::now();
    while !dir.join("wayland-1").exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "compositor never came up"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    g
}

/// `apoth server kill NAME` (the session daemons the windows started on their own).
fn kill_session(apoth: &Path, dir: &Path, name: &str) -> Option<i32> {
    let mut k = Command::new(apoth);
    base_env(&mut k, dir);
    k.args(["server", "kill", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .code()
}

fn rpc_line(s: &mut BufReader<UnixStream>, id: u32, method: &str, params: &str) -> String {
    let line = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\",\"params\":{params}}}\n"
    );
    let _ = s.get_mut().write_all(line.as_bytes());
    let mut out = String::new();
    let _ = s.read_line(&mut out);
    out
}

fn abyss_rpc(sock: &Path, method: &str, params: &str) {
    let Ok(s) = UnixStream::connect(sock) else {
        return;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = rpc_line(&mut BufReader::new(s), 1, method, params);
}

fn wait_file(p: &Path, secs: u64) -> bool {
    let t0 = Instant::now();
    while !p.exists() {
        if t0.elapsed() > Duration::from_secs(secs) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

fn pids(dir: &Path) -> Vec<i32> {
    std::fs::read_to_string(dir.join("ssh.pids"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// Wait until the GUI's dump shows `text`; `None` if the GUI found no GPU (skip).
fn dump_shows(g: &mut Guard, dump: &Path, text: &str, secs: u64) -> Option<bool> {
    let t0 = Instant::now();
    loop {
        if std::fs::read_to_string(dump).is_ok_and(|t| t.contains(text)) {
            return Some(true);
        }
        if let Ok(Some(st)) = g.0.try_wait() {
            if st.code() == Some(69) && std::env::var_os("APOTH_REQUIRE_GPU").is_none() {
                eprintln!("skipping: no usable GPU");
                return None;
            }
            panic!("apotheosis --domain exited: {st}");
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            return Some(false);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Type `cmd` until the dump shows `want` (the first lines may land before the prompt).
fn type_until(g: &mut Guard, dir: &Path, cmd: &str, want: &str) -> Option<bool> {
    let sock = dir.join("sock/abyss.sock");
    let text = format!("{{\"text\":\"{}\\n\"}}", cmd.replace('"', "\\\""));
    let mut shown = Some(false);
    for _ in 0..10 {
        abyss_rpc(&sock, "type_text", &text);
        shown = dump_shows(g, &dir.join("dump.txt"), want, 3);
        if shown != Some(false) {
            break;
        }
    }
    shown
}

#[test]
fn persist_always_without_apoth_on_the_host_says_why_and_off_never_asks() {
    let Some(apoth) = apoth_bin() else { return };
    let dir = temp_dir("modes");
    let path = shim_path(&dir, &apoth);
    std::fs::write(dir.join("no-apoth"), b"").unwrap();
    // always: the host must run the session daemon; it does not, so the window says why.
    write_config(
        &dir,
        "domain \"box\" {\n  ssh \"me@box\"\n  persist \"always\"\n}\n",
    );
    let o = gui(&dir, &path, &["--domain", "box"], false)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("me@box") && err.contains("apoth") && err.contains("persist \"always\""),
        "{err}"
    );
    assert!(dir.join("probes").exists(), "always asks the host");
    assert!(!dir.join("proxied").exists() && pids(&dir).is_empty());

    // off (the key; the domain says nothing): never probed, never the host's daemon. With
    // the daemon on, the pane is its: `apoth ssh` runs there even though no window opens.
    let _ = std::fs::remove_file(dir.join("probes"));
    pre_consent(&dir);
    write_config(
        &dir,
        "remote {\n  persist \"off\"\n}\nsessions {\n  server \"on\"\n}\ndomain \"box\" {\n  ssh \"me@box\"\n}\n",
    );
    let _ = gui(&dir, &path, &["--domain", "box"], false)
        .status()
        .unwrap();
    assert!(
        wait_file(&dir.join("ssh.pids"), 20),
        "the local daemon's pane never ran ssh"
    );
    assert!(!dir.join("probes").exists(), "off never asks the host");
    assert!(!dir.join("proxied").exists());
    assert!(dir.join("apotheosis/server-ssh-box.sock").exists());
    assert_eq!(kill_session(&apoth, &dir, "ssh-box"), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_mux_domain_window_asks_on_the_sheet_and_survives_a_dropped_link() {
    let Some(abyss) = find_abyss() else { return };
    let Some(apoth) = apoth_bin() else { return };
    let dir = temp_dir("gui");
    let path = shim_path(&dir, &apoth);
    // auto (the default) and the host has `apoth`: the host's session daemon.
    write_config(&dir, "domain \"box\" { ssh \"me@box\" }\n");
    std::fs::write(dir.join("askpass.want"), b"").unwrap();
    let _abyss = start_abyss(&abyss, &dir);
    let dump = dir.join("dump.txt");
    let mut g = Guard(
        gui(&dir, &path, &["--domain", "box"], true)
            .spawn()
            .expect("spawn apotheosis --domain"),
    );
    let abyss_sock = dir.join("sock/abyss.sock");

    // The fake ssh asks for a password: the sheet takes it from the seat.
    assert!(wait_file(&dir.join("askpass.asked"), 30), "ssh never asked");
    assert!(dir.join("probes").exists(), "auto asked the host first");
    std::thread::sleep(Duration::from_millis(1500));
    if let Ok(Some(st)) = g.0.try_wait()
        && st.code() == Some(69)
        && std::env::var_os("APOTH_REQUIRE_GPU").is_none()
    {
        eprintln!("skipping: no usable GPU");
        return;
    }
    abyss_rpc(&abyss_sock, "type_text", "{\"text\":\"s3cret-M37\\n\"}");
    assert!(
        wait_file(&dir.join("askpass.ok"), 15),
        "the sheet's answer never reached ssh (bad: {}, cancelled: {})",
        dir.join("askpass.bad").exists(),
        dir.join("askpass.cancelled").exists()
    );
    assert!(dir.join("proxied").exists(), "the link runs apoth proxy");

    // The JSON socket never arms askpass for a process outside the panes.
    let gui_sock = dir.join(format!("apotheosis/{}.sock", g.0.id()));
    assert!(wait_file(&gui_sock, 10), "no control socket");
    let s = UnixStream::connect(&gui_sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut s = BufReader::new(s);
    rpc_line(&mut s, 1, "hello", "{\"proto\":1,\"client\":\"test\"}");
    let r = rpc_line(&mut s, 2, "askpass.arm", "{\"host\":\"me@box\"}");
    assert!(r.contains("\"error\"") && !r.contains("token"), "{r}");
    drop(s);

    // Through the link: the pane is the host's daemon's (the proxy pre-grant, no hook).
    let bgfile = dir.join("bg.pid");
    let cmd = format!(
        "sleep 300 & echo $! > {}; echo m37-$((6*7))",
        bgfile.display()
    );
    let Some(shown) = type_until(&mut g, &dir, &cmd, "m37-42") else {
        return;
    };
    assert!(shown, "the remote pane never echoed");
    let bg: i32 = std::fs::read_to_string(&bgfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // Drop the link: kill the fake ssh (it recorded its own pid; it is the proxy now).
    let first = *pids(&dir).last().expect("the ssh pid");
    let _ = Command::new("kill").arg(first.to_string()).status();
    let t0 = Instant::now();
    while pids(&dir).len() < 2 {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "the window never reconnected"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // The process and the output are still there after the reconnect.
    let alive = type_until(
        &mut g,
        &dir,
        &format!("kill -0 {bg} && echo alive-$((40+2))"),
        "alive-42",
    );
    assert_eq!(alive, Some(true), "the pane's job did not survive the drop");
    assert!(
        std::fs::read_to_string(&dump).is_ok_and(|t| t.contains("m37-42")),
        "the output from before the drop is still shown"
    );

    // End the session (tmux kill-session): every pane goes, the daemon exits.
    assert_eq!(kill_session(&apoth, &dir, "default"), Some(0));
    // A background job of an interactive shell may outlive its hang-up (as under tmux):
    // ours is removed by the pid it reported.
    std::thread::sleep(Duration::from_millis(300));
    if Path::new(&format!("/proc/{bg}")).exists() {
        let _ = Command::new("kill").arg(bg.to_string()).status();
    }
    drop(g);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_apoth_on_the_host_the_local_daemon_keeps_the_ssh_pane_past_a_window_kill() {
    let Some(abyss) = find_abyss() else { return };
    let Some(apoth) = apoth_bin() else { return };
    let dir = temp_dir("local");
    let path = shim_path(&dir, &apoth);
    std::fs::write(dir.join("no-apoth"), b"").unwrap();
    pre_consent(&dir);
    write_config(
        &dir,
        "sessions {\n  server \"on\"\n}\ndomain \"box\" {\n  ssh \"me@box\"\n}\n",
    );
    let _abyss = start_abyss(&abyss, &dir);
    let dump = dir.join("dump.txt");
    let mut g = Guard(
        gui(&dir, &path, &["--domain", "box"], true)
            .spawn()
            .expect("spawn apotheosis --domain"),
    );
    let Some(shown) = type_until(&mut g, &dir, "echo local-$((8*8))", "local-64") else {
        return;
    };
    assert!(shown, "the ssh pane never answered");
    assert!(dir.join("probes").exists(), "auto asked the host");
    assert!(
        !dir.join("proxied").exists(),
        "no apoth on the host: no proxy"
    );
    assert!(dir.join("apotheosis/server-ssh-box.sock").exists());
    let ssh = pids(&dir);
    assert_eq!(ssh.len(), 1);

    // The window dies; the daemon's pane (and its ssh) does not.
    drop(g);
    std::thread::sleep(Duration::from_millis(500));
    assert!(Path::new(&format!("/proc/{}", ssh[0])).exists(), "ssh died");
    let _ = std::fs::remove_file(&dump);
    let mut g = Guard(
        gui(&dir, &path, &["--domain", "box"], true)
            .spawn()
            .expect("spawn apotheosis --domain again"),
    );
    match dump_shows(&mut g, &dump, "local-64", 30) {
        None => return,
        Some(s) => assert!(s, "the new window does not show the pane's earlier output"),
    }
    assert_eq!(
        pids(&dir).len(),
        1,
        "the pane was reattached, not restarted"
    );
    drop(g);
    assert_eq!(kill_session(&apoth, &dir, "ssh-box"), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_ssh_domain_runs_apoth_ssh_with_askpass_armed_and_reconnects_after_a_drop() {
    let Some(abyss) = find_abyss() else { return };
    let Some(apoth) = apoth_bin() else { return };
    let dir = temp_dir("plain");
    let path = shim_path(&dir, &apoth);
    std::fs::write(dir.join("no-apoth"), b"").unwrap();
    write_config(
        &dir,
        "binds {\n  bind \"ctrl+shift+r\" { pane.reconnect }\n}\ndomain \"plain\" {\n  ssh \"me@box\"\n}\n",
    );
    let _abyss = start_abyss(&abyss, &dir);
    let dump = dir.join("dump.txt");
    let mut g = Guard(
        gui(&dir, &path, &["--domain", "plain", "--new-instance"], true)
            .spawn()
            .expect("spawn apotheosis --domain"),
    );
    let abyss_sock = dir.join("sock/abyss.sock");
    // `apoth ssh` asks once per host on the terminal whether to carry the environment.
    match dump_shows(&mut g, &dump, "send COLORTERM", 40) {
        None => return,
        Some(shown) => assert!(shown, "apoth ssh never asked"),
    }
    abyss_rpc(&abyss_sock, "type_text", "{\"text\":\"n\\n\"}");
    assert!(wait_file(&dir.join("ssh.pids"), 15), "ssh never ran");
    assert!(
        dir.join("askpass.armed").exists(),
        "askpass is armed for a process in the pane"
    );
    assert!(!dir.join("proxied").exists());
    let consent = std::fs::read_to_string(dir.join("home/.state/apotheosis/ssh-hosts.kdl"))
        .expect("the answer is recorded");
    assert!(
        consent.contains("me@box") && consent.contains("env=#false"),
        "{consent}"
    );
    assert_eq!(
        type_until(&mut g, &dir, "echo plain-$((5*5))", "plain-25"),
        Some(true),
        "the domain's shell never answered"
    );

    // The link drops (ssh exits 255): the pane holds, says so, and Reconnect starts a
    // fresh ssh in it.
    abyss_rpc(&abyss_sock, "type_text", "{\"text\":\"exit 255\\n\"}");
    assert_eq!(
        dump_shows(&mut g, &dump, "was lost", 15),
        Some(true),
        "the pane never said the link was lost"
    );
    std::thread::sleep(Duration::from_millis(600));
    abyss_rpc(
        &abyss_sock,
        "press_key",
        "{\"key\":\"r\",\"modifiers\":[\"ctrl\",\"shift\"]}",
    );
    let t0 = Instant::now();
    while pids(&dir).len() < 2 {
        assert!(
            t0.elapsed() < Duration::from_secs(15),
            "Reconnect never started a new ssh"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        type_until(&mut g, &dir, "echo again-$((6*6))", "again-36"),
        Some(true),
        "the reconnected shell never answered"
    );
    drop(g);
    let _ = std::fs::remove_dir_all(&dir);
}
