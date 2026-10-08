// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-secret account ...` end to end: the real binary against a fake brokerd
//! (a SEQPACKET listener speaking brokerd's own wire types) and, for `login`,
//! a fake `claude` shell script that prints the byte shapes `claude
//! setup-token` draws under a pty.

use ec_brokerd::broker::Status;
use ec_brokerd::wire::{MetaView, Request, Response, MAX_MESSAGE};
use rustix::net::{self, AddressFamily, RecvFlags, SendFlags, SocketAddrUnix, SocketFlags, SocketType};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TOKEN: &str = "sk-ant-oat01-TestTokenValue_0123456789-abcdefghijklmnop";
const URL: &str = "https://claude.com/cai/oauth/authorize?code=true&client_id=abc&state=xyz";

static N: AtomicU32 = AtomicU32::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Scratch {
        let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "acct-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("eclipse")).unwrap();
        Scratch(d)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Broker {
    locked: AtomicBool,
    uninit: AtomicBool,
    /// (name, rotation counter, last value)
    secrets: Mutex<Vec<(String, u64, Vec<u8>)>>,
    /// "add <name>" / "rotate <name>" / "revoke <name>"
    ops: Mutex<Vec<String>>,
}

fn serve(sock: &Path, b: Arc<Broker>) {
    let fd = net::socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    net::bind(&fd, &SocketAddrUnix::new(sock).unwrap()).unwrap();
    net::listen(&fd, 8).unwrap();
    std::thread::spawn(move || loop {
        let Ok(c) = net::accept(&fd) else { return };
        let b = b.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; MAX_MESSAGE];
            loop {
                let Ok((n, _)) = net::recv(&c, &mut buf[..], RecvFlags::empty()) else {
                    return;
                };
                if n == 0 {
                    return;
                }
                let resp = match Request::decode(&buf[..n]) {
                    Ok(r) => answer(&b, r),
                    Err(_) => Response::Err(Status::InvalidArgument.code()),
                };
                if net::send(&c, &resp.encode(), SendFlags::NOSIGNAL).is_err() {
                    return;
                }
            }
        });
    });
}

fn answer(b: &Broker, r: Request) -> Response {
    let locked = b.locked.load(Ordering::Relaxed);
    let mut s = b.secrets.lock().unwrap();
    match r {
        Request::Status => Response::State {
            unlocked: !locked && !b.uninit.load(Ordering::Relaxed),
            initialised: !b.uninit.load(Ordering::Relaxed),
            passphrase: true,
        },
        _ if locked => Response::Err(Status::BrokerLocked.code()),
        Request::List => Response::List(
            s.iter()
                .map(|(name, rot, _)| MetaView {
                    name: name.clone(),
                    id: [0; 16],
                    kind: "bearer".into(),
                    bound_to: vec!["host:api.anthropic.com".into()],
                    modes: vec!["proxy_header".into()],
                    requires_prompt: false,
                    rotation_counter: *rot,
                })
                .collect(),
        ),
        Request::Add { spec, value } => {
            assert_eq!(spec.bound_to.len(), 1);
            assert_eq!(spec.bound_to[0].to_text(), "host:api.anthropic.com:443");
            b.ops.lock().unwrap().push(format!("add {}", spec.name));
            s.push((spec.name, 0, value.as_slice().to_vec()));
            Response::Ok
        }
        Request::Rotate { name, value } => {
            b.ops.lock().unwrap().push(format!("rotate {name}"));
            match s.iter_mut().find(|e| e.0 == name) {
                Some(e) => {
                    e.1 += 1;
                    e.2 = value.as_slice().to_vec();
                    Response::Ok
                }
                None => Response::Err(Status::NoCapability.code()),
            }
        }
        Request::Revoke { name } => {
            b.ops.lock().unwrap().push(format!("revoke {name}"));
            let before = s.len();
            s.retain(|e| e.0 != name);
            if s.len() == before {
                Response::Err(Status::NoCapability.code())
            } else {
                Response::Ok
            }
        }
        _ => Response::Err(Status::InvalidArgument.code()),
    }
}

struct Rig {
    dir: Scratch,
    broker: Arc<Broker>,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        let dir = Scratch::new(tag);
        let broker = Arc::new(Broker::default());
        serve(&dir.0.join("eclipse/brokerd.sock"), broker.clone());
        Rig { dir, broker }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ec-secret"));
        c.args(args)
            .env("XDG_RUNTIME_DIR", &self.dir.0)
            .env_remove("EC_SECRET_CLAUDE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    /// Temp dirs `login` left in the runtime dir.
    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(&self.dir.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("ec-secret-login-"))
            .collect()
    }

    /// A fake `claude` that behaves like `setup-token`: prints the banner, the
    /// URL as an OSC 8 hyperlink and the prompt, reads one line, and prints the
    /// token only for the right code. `tail` runs after a good code.
    fn fake_claude(&self, tail: &str) -> PathBuf {
        let report = self.dir.0.join("report");
        let script = format!(
            r#"#!/bin/sh
{{ echo "pid=$$"; echo "cfg=$CLAUDE_CONFIG_DIR"; echo "browser=$BROWSER"; echo "args=$*"; echo "size=$(stty size)"; \
   echo "cfgmode=$(stat -c %a "$CLAUDE_CONFIG_DIR")"; echo "tty=$(test -t 0 && echo yes)"; }} > '{report}'
printf '\033[?25l\033[1mWelcome to Claude Code\033[22m\r\n'
printf '\033]8;id=p3gklx;{url}\007\033[38;5;246m{url}\033[39m\033]8;;\007\r\r\n'
printf '\033[2GPaste\033[8Gcode\033[13Ghere\033[18Gif\033[21Gprompted\033[30G>'
IFS= read -r code
if [ "$code" = "good-code#state" ]; then
  printf '\r\n\033[2GYour\033[7Goauth\033[13Gtoken:\r\n\033[1m%s\033[22m\r\n\033[2GStore\033[8Gthis\r\n' '{token}'
  {tail}
else
  printf '\r\nInvalid code\r\n'
  exit 1
fi
"#,
            report = report.display(),
            url = URL,
            token = TOKEN,
            tail = tail
        );
        let p = self.dir.0.join("claude");
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn report(&self) -> String {
        std::fs::read_to_string(self.dir.0.join("report")).unwrap_or_default()
    }
}

fn read_line(r: &mut BufReader<impl Read>) -> String {
    let mut l = String::new();
    r.read_line(&mut l).unwrap();
    l.trim_end().to_owned()
}

fn finish(mut c: Child) -> (i32, String, String) {
    let t = Instant::now();
    let status = loop {
        if let Some(s) = c.try_wait().unwrap() {
            break s;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "ec-secret did not exit");
        std::thread::sleep(Duration::from_millis(20));
    };
    let (mut o, mut e) = (String::new(), String::new());
    c.stdout.take().unwrap().read_to_string(&mut o).unwrap();
    c.stderr.take().unwrap().read_to_string(&mut e).unwrap();
    (status.code().unwrap_or(-1), o, e)
}

fn alive(pid: &str) -> bool {
    // A zombie (waiting for its reaper) is gone for our purposes.
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z"))
}

fn pid_of(report: &str) -> String {
    report
        .lines()
        .find_map(|l| l.strip_prefix("pid="))
        .expect("pid in report")
        .to_owned()
}

#[test]
fn login_json_emits_events_stores_the_token_and_prints_it_nowhere() {
    let rig = Rig::new("login");
    let claude = rig.fake_claude("sleep 60 & sleep 60");
    let mut c = rig
        .cmd(&["account", "login", "work", "--json"])
        .env("EC_SECRET_CLAUDE", &claude)
        .spawn()
        .unwrap();
    let mut out = BufReader::new(c.stdout.take().unwrap());
    assert_eq!(
        read_line(&mut out),
        format!("{{\"event\":\"url\",\"url\":\"{URL}\"}}")
    );
    assert_eq!(read_line(&mut out), "{\"event\":\"code_needed\"}");
    c.stdin.take().unwrap().write_all(b"good-code#state\n").unwrap();
    assert_eq!(read_line(&mut out), "{\"event\":\"stored\",\"account\":\"work\"}");
    c.stdout = Some(out.into_inner());
    let (code, rest, err) = finish(c);
    assert_eq!(code, 0, "{err}");
    assert_eq!(rest, "");
    assert!(!err.contains("sk-ant"), "{err}");

    let secrets = rig.broker.secrets.lock().unwrap();
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0].0, "claude-code-token.work");
    assert_eq!(secrets[0].2, TOKEN.as_bytes());
    assert_eq!(*rig.broker.ops.lock().unwrap(), ["add claude-code-token.work"]);

    let rep = rig.report();
    assert!(rep.contains("browser=true"), "{rep}");
    assert!(rep.contains("args=setup-token"), "{rep}");
    assert!(rep.contains("size=50 500"), "{rep}");
    assert!(rep.contains("cfgmode=700"), "{rep}");
    assert!(rep.contains("tty=yes"), "{rep}");
    let cfg = rep.lines().find_map(|l| l.strip_prefix("cfg=")).unwrap();
    assert!(cfg.contains("ec-secret-login-"), "{cfg}");
    assert!(!Path::new(cfg).exists(), "the private config dir is removed");
    assert!(rig.leftovers().is_empty());
    assert!(!alive(&pid_of(&rep)), "claude was killed");
}

#[test]
fn an_existing_token_is_rotated_and_the_default_account_has_the_bare_name() {
    let rig = Rig::new("rotate");
    rig.broker
        .secrets
        .lock()
        .unwrap()
        .push(("claude-code-token".into(), 4, b"old".to_vec()));
    let claude = rig.fake_claude("exit 0");
    let mut c = rig
        .cmd(&["account", "login", "default", "--json"])
        .env("EC_SECRET_CLAUDE", &claude)
        .spawn()
        .unwrap();
    let mut out = BufReader::new(c.stdout.take().unwrap());
    read_line(&mut out);
    read_line(&mut out);
    c.stdin.take().unwrap().write_all(b"good-code#state\n").unwrap();
    assert_eq!(
        read_line(&mut out),
        "{\"event\":\"stored\",\"account\":\"default\"}"
    );
    c.stdout = Some(out.into_inner());
    assert_eq!(finish(c).0, 0);
    assert_eq!(*rig.broker.ops.lock().unwrap(), ["rotate claude-code-token"]);
    let s = rig.broker.secrets.lock().unwrap();
    assert_eq!((s[0].1, s[0].2.as_slice()), (5, TOKEN.as_bytes()));
}

fn error_of(args: &[&str], rig: &Rig, claude: Option<&Path>, stdin: &str) -> (i32, Vec<String>) {
    let mut cmd = rig.cmd(args);
    if let Some(p) = claude {
        cmd.env("EC_SECRET_CLAUDE", p);
    }
    let mut c = cmd.spawn().unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let (code, out, err) = finish(c);
    assert!(!out.contains("sk-ant") && !err.contains("sk-ant"));
    (code, out.lines().map(str::to_owned).collect())
}

#[test]
fn a_wrong_code_is_no_token() {
    let rig = Rig::new("badcode");
    let claude = rig.fake_claude("exit 0");
    let (code, lines) = error_of(
        &["account", "login", "work", "--json"],
        &rig,
        Some(&claude),
        "nope\n",
    );
    assert_eq!(code, 1);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines[2].starts_with("{\"event\":\"error\",\"code\":\"no_token\""),
        "{lines:?}"
    );
    assert!(rig.broker.secrets.lock().unwrap().is_empty());
    assert!(rig.leftovers().is_empty());
}

#[test]
fn login_refuses_before_running_claude_when_it_cannot_store() {
    let rig = Rig::new("refuse");
    let claude = rig.fake_claude("exit 0");

    rig.broker.locked.store(true, Ordering::Relaxed);
    let (code, lines) = error_of(&["account", "login", "work", "--json"], &rig, Some(&claude), "");
    assert_eq!((code, lines.len()), (1, 1));
    assert!(
        lines[0].starts_with("{\"event\":\"error\",\"code\":\"locked\""),
        "{lines:?}"
    );

    rig.broker.locked.store(false, Ordering::Relaxed);
    rig.broker.uninit.store(true, Ordering::Relaxed);
    let (_, lines) = error_of(&["account", "login", "work", "--json"], &rig, Some(&claude), "");
    assert!(lines[0].contains("\"code\":\"uninitialised\""), "{lines:?}");
    rig.broker.uninit.store(false, Ordering::Relaxed);

    let (code, lines) = error_of(&["account", "login", "a.b", "--json"], &rig, Some(&claude), "");
    assert_eq!(code, 2);
    assert!(lines[0].contains("\"code\":\"bad_account\""), "{lines:?}");

    let (_, lines) = error_of(
        &["account", "login", "work", "--json"],
        &rig,
        Some(Path::new("/nonexistent/claude")),
        "",
    );
    assert!(lines[0].contains("\"code\":\"no_claude\""), "{lines:?}");

    assert_eq!(rig.report(), "", "claude never ran");
}

#[test]
fn the_overall_timeout_kills_claude_and_cleans_up() {
    let rig = Rig::new("timeout");
    let claude = rig.fake_claude("exit 0");
    let mut c = rig
        .cmd(&["account", "login", "work", "--json"])
        .env("EC_SECRET_CLAUDE", &claude)
        .env("EC_SECRET_LOGIN_TIMEOUT_MS", "700")
        .spawn()
        .unwrap();
    let stdin = c.stdin.take().unwrap(); // held open, no code ever sent
    let (code, out, _) = finish(c);
    drop(stdin);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(code, 1, "{out}");
    assert_eq!(lines.len(), 3, "{out}");
    assert!(lines[2].contains("\"code\":\"timeout\""), "{out}");
    assert!(rig.leftovers().is_empty());
    assert!(!alive(&pid_of(&rig.report())), "claude was killed");
}

#[test]
fn sigterm_cancels_a_login_and_cleans_up() {
    let rig = Rig::new("term");
    let claude = rig.fake_claude("exit 0");
    let mut c = rig
        .cmd(&["account", "login", "work", "--json"])
        .env("EC_SECRET_CLAUDE", &claude)
        .spawn()
        .unwrap();
    let stdin = c.stdin.take().unwrap();
    let mut out = BufReader::new(c.stdout.take().unwrap());
    read_line(&mut out);
    assert_eq!(read_line(&mut out), "{\"event\":\"code_needed\"}");
    rustix::process::kill_process(
        rustix::process::Pid::from_child(&c),
        rustix::process::Signal::TERM,
    )
    .unwrap();
    c.stdout = Some(out.into_inner());
    let (code, rest, _) = finish(c);
    drop(stdin);
    assert_eq!(code, 1);
    assert!(rest.contains("\"code\":\"cancelled\""), "{rest}");
    assert!(rig.leftovers().is_empty());
    assert!(!alive(&pid_of(&rig.report())));
}

#[test]
fn list_json_shows_only_accounts_and_never_fails_when_locked() {
    let rig = Rig::new("list");
    {
        let mut s = rig.broker.secrets.lock().unwrap();
        s.push(("claude-code-token".into(), 0, vec![]));
        s.push(("anthropic-api-key.work".into(), 1, vec![]));
        s.push(("something-else".into(), 0, vec![]));
    }
    let (code, lines) = error_of(&["account", "list", "--json"], &rig, None, "");
    assert_eq!(code, 0);
    assert_eq!(
        lines,
        [
            r#"{"initialised":true,"locked":false,"accounts":[{"name":"default","kind":"claude-code","rotations":0},{"name":"work","kind":"api-key","rotations":1}]}"#
        ]
    );
    rig.broker.locked.store(true, Ordering::Relaxed);
    let (code, lines) = error_of(&["account", "list", "--json"], &rig, None, "");
    assert_eq!(code, 0);
    assert_eq!(lines, [r#"{"initialised":true,"locked":true,"accounts":[]}"#]);
    rig.broker.uninit.store(true, Ordering::Relaxed);
    let (_, lines) = error_of(&["account", "list", "--json"], &rig, None, "");
    assert_eq!(lines, [r#"{"initialised":false,"locked":true,"accounts":[]}"#]);
}

#[test]
fn list_json_with_no_brokerd_is_an_unavailable_error() {
    let dir = Scratch::new("down");
    let o = Command::new(env!("CARGO_BIN_EXE_ec-secret"))
        .args(["account", "list", "--json"])
        .env("XDG_RUNTIME_DIR", &dir.0)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(3));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.starts_with("{\"error\":\"unavailable\",\"message\":"),
        "{out}"
    );
}

#[test]
fn add_key_stores_and_remove_revokes() {
    let rig = Rig::new("key");
    let (code, _) = error_of(
        &["account", "add-key", "work", "--stdin"],
        &rig,
        None,
        "sk-ant-api03-SECRETKEY\n",
    );
    assert_eq!(code, 0);
    let (code, _) = error_of(
        &["account", "add-key", "work", "--stdin"],
        &rig,
        None,
        "sk-ant-api03-SECOND\n",
    );
    assert_eq!(code, 0);
    assert_eq!(
        *rig.broker.ops.lock().unwrap(),
        ["add anthropic-api-key.work", "rotate anthropic-api-key.work"]
    );
    assert_eq!(rig.broker.secrets.lock().unwrap()[0].2, b"sk-ant-api03-SECOND");

    let (code, lines) = error_of(
        &["account", "remove", "work", "--kind", "api-key", "--json"],
        &rig,
        None,
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(lines, [r#"{"removed":"work","kind":"api-key"}"#]);
    assert!(rig.broker.secrets.lock().unwrap().is_empty());
    let (code, lines) = error_of(
        &["account", "remove", "work", "--kind", "api-key", "--json"],
        &rig,
        None,
        "",
    );
    assert_eq!(code, 1);
    assert!(lines[0].starts_with("{\"error\":\"refused\""), "{lines:?}");
}

#[test]
fn stdin_eof_before_a_code_cancels_and_cleans_up() {
    let rig = Rig::new("eof");
    let claude = rig.fake_claude("exit 0");
    let mut c = rig
        .cmd(&["account", "login", "work", "--json"])
        .env("EC_SECRET_CLAUDE", &claude)
        .spawn()
        .unwrap();
    drop(c.stdin.take()); // closed at once: no code will ever come
    let (code, out, _) = finish(c);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("\"code\":\"cancelled\""), "{out}");
    assert!(rig.leftovers().is_empty());
    assert!(rig.broker.secrets.lock().unwrap().is_empty());
}
