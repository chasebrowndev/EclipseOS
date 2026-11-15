// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth ssh [--hold] [--cwd DIR] [SSH-OPTIONS...] DESTINATION [COMMAND...]` (T-22 §2-§4): the
//! system `ssh` with a private shared master, a stale-master probe, the per-host features
//! (terminfo entry, environment carry; each consented once per host and recorded in KDL),
//! and the armed askpass channel when run inside an Apotheosis pane.
//!
//! The process `exec`s ssh at the end, so the process the instance armed askpass for *is*
//! the ssh process (its pid, its pidfd, its lifetime; T-22 §3). With `--hold` (what an ssh
//! domain's panes run, T-22 §4) it stays as ssh's parent instead: when the link drops (ssh
//! exit 255) the pane keeps its process, says "connection lost", sets the user variable
//! `apoth_ssh_lost` (OSC 1337, the host) for the window's Reconnect notice, and starts a
//! fresh ssh on Enter (Ctrl-D closes). ssh descends from the armed process, so askpass
//! keeps working across reconnects. Every step before that
//! falls back to plain ssh semantics on failure; nothing is downloaded (T-22 §2). Nothing
//! here logs the command line, the token or a secret.

use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use apoth_proto::askpass::{ENV_SOCK, ENV_TOKEN};
use apoth_proto::params::{AskpassArm, Hello};
use apoth_proto::results::AskpassArmReply;
use apoth_proto::{Params, message::PROTO_VERSION};
use apoth_pty::ssh as sshx;

use crate::ctl::{ERROR, USAGE};

/// The terminfo entry `apoth ssh` ships.
const ENTRY: &str = "xterm-apotheosis";
/// Bound on the terminfo install round trip.
const INSTALL_WITHIN: Duration = Duration::from_secs(15);
/// Hosts recorded in the consent file.
const MAX_HOSTS: usize = 1024;

/// What the command line says.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Args {
    /// Stay as ssh's parent and offer a reconnect when the link drops.
    pub(crate) hold: bool,
    pub(crate) cwd: Option<String>,
    /// Everything for ssh, unchanged.
    pub(crate) ssh: Vec<String>,
    pub(crate) dest: String,
    /// A remote command follows the destination.
    pub(crate) has_command: bool,
}

pub(crate) fn parse(args: &[String]) -> Result<Args, String> {
    let mut rest = args;
    let mut cwd = None;
    let mut hold = false;
    loop {
        match rest {
            [flag, v, tail @ ..] if flag == "--cwd" => {
                cwd = Some(v.clone());
                rest = tail;
            }
            [flag, tail @ ..] if flag == "--hold" => {
                hold = true;
                rest = tail;
            }
            _ => break,
        }
    }
    let i = sshx::destination_index(rest).ok_or("missing DESTINATION")?;
    let dest = rest[i].clone();
    if !sshx::valid_destination(&dest) {
        return Err("DESTINATION must be 1-255 bytes, without a leading `-` or spaces".into());
    }
    Ok(Args {
        hold,
        cwd,
        ssh: rest.to_vec(),
        dest,
        has_command: i + 1 < rest.len(),
    })
}

/// One host's recorded answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostConsent {
    pub(crate) host: String,
    pub(crate) terminfo: Option<bool>,
    pub(crate) env: Option<bool>,
}

/// Hosts that can be written back: no quote or backslash (the file is plain KDL).
fn recordable(h: &str) -> bool {
    sshx::valid_destination(h) && !h.contains(['"', '\\'])
}

/// Parse the consent file: `host "<dest>" terminfo=#true env=#false` lines; anything else
/// is ignored.
pub(crate) fn parse_consent(text: &str) -> Vec<HostConsent> {
    let mut v = Vec::new();
    for line in text.lines().take(4 * MAX_HOSTS) {
        let Some(rest) = line.trim().strip_prefix("host \"") else {
            continue;
        };
        let Some(end) = rest.find('"') else { continue };
        let host = &rest[..end];
        if !recordable(host) {
            continue;
        }
        let mut c = HostConsent {
            host: host.to_owned(),
            ..HostConsent::default()
        };
        for prop in rest[end + 1..].split_whitespace() {
            let (k, val) = match prop.split_once('=') {
                Some((k, "#true")) => (k, true),
                Some((k, "#false")) => (k, false),
                _ => continue,
            };
            match k {
                "terminfo" => c.terminfo = Some(val),
                "env" => c.env = Some(val),
                _ => {}
            }
        }
        v.retain(|x: &HostConsent| x.host != c.host);
        v.push(c);
        if v.len() >= MAX_HOSTS {
            break;
        }
    }
    v
}

pub(crate) fn render_consent(v: &[HostConsent]) -> String {
    let b = |o: Option<bool>| o.map(|b| if b { "#true" } else { "#false" });
    let mut out = String::from(
        "// apoth ssh: per-host answers (T-22 §2). Delete a line to be asked again.\n",
    );
    for c in v.iter().filter(|c| recordable(&c.host)).take(MAX_HOSTS) {
        out.push_str(&format!("host \"{}\"", c.host));
        if let Some(t) = b(c.terminfo) {
            out.push_str(&format!(" terminfo={t}"));
        }
        if let Some(e) = b(c.env) {
            out.push_str(&format!(" env={e}"));
        }
        out.push('\n');
    }
    out
}

/// `$XDG_STATE_HOME/apotheosis/ssh-hosts.kdl` (else `~/.local/state/…`).
fn consent_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".local/state"))
        })?;
    Some(base.join("apotheosis/ssh-hosts.kdl"))
}

fn load_consent(p: &Path) -> Vec<HostConsent> {
    use std::io::Read;
    let mut s = String::new();
    match std::fs::File::open(p) {
        Ok(f) => {
            let _ = f.take(256 * 1024).read_to_string(&mut s);
            parse_consent(&s)
        }
        Err(_) => Vec::new(),
    }
}

fn save_consent(p: &Path, v: &[HostConsent]) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("kdl.tmp");
    if std::fs::write(&tmp, render_consent(v)).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

/// Ask a yes/no question on the terminal (default no).
fn ask(question: &str) -> bool {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "apoth ssh: {question} [y/N] ");
    let _ = err.flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
}

/// Arm askpass over the control socket (T-22 §3). `None`: not inside one of our panes, no
/// instance, or refused: then ssh gets no askpass from us.
fn arm(host: &str) -> Option<AskpassArmReply> {
    std::env::var_os(apoth_proto::ENV_SOCKET).filter(|s| !s.is_empty())?;
    let stream = crate::ctl::connect(None).ok()?;
    let mut c = crate::ctl::Client::new(stream).ok()?;
    c.call(Params::Hello(Hello {
        proto: PROTO_VERSION,
        client: "apoth ssh".into(),
        want: Vec::new(),
    }))
    .ok()?;
    match c.call(Params::AskpassArm(AskpassArm {
        host: host.to_owned(),
    })) {
        Ok(v) => serde_json::from_value(v).ok(),
        Err(e) => {
            eprintln!("apoth ssh: no askpass sheet ({})", e.message);
            None
        }
    }
}

pub(crate) fn run(args: &[String]) -> i32 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(m) => {
            eprintln!(
                "apoth ssh: {m}\nusage: apoth ssh [--cwd DIR] [SSH-OPTIONS...] DESTINATION [COMMAND...]"
            );
            return USAGE;
        }
    };
    let ssh = PathBuf::from("ssh");
    // The private master (REMOTE-08/-09); no directory = plain ssh.
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .and_then(|rt| sshx::prepare_control_dir(&rt, rustix::process::getuid().as_raw()).ok());
    let master = sshx::master_args(dir.as_deref());
    if dir.is_some() {
        let _ = sshx::probe_master(ssh.as_os_str(), &master, &a.dest, sshx::PROBE_TIMEOUT);
    }
    // Askpass first: the terminfo install below may be the one that opens the master.
    let armed = arm(&a.dest);
    let set_askpass = |c: &mut Command| {
        c.env_remove(ENV_SOCK).env_remove(ENV_TOKEN);
        if let (Some(r), Ok(me)) = (&armed, std::env::current_exe()) {
            c.env("SSH_ASKPASS", me)
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env(ENV_SOCK, &r.socket)
                .env(ENV_TOKEN, &r.token);
        }
    };
    // Per-host features, consented once (T-22 §2).
    let path = consent_path();
    let mut hosts = path.as_deref().map(load_consent).unwrap_or_default();
    let tty = std::io::stdin().is_terminal();
    let local_term = std::env::var("TERM").ok();
    let entry = (local_term.as_deref() == Some(ENTRY))
        .then(|| sshx::compiled_entry(&apoth_pty::terminfo_dirs(&|k| std::env::var_os(k)), ENTRY))
        .flatten();
    let mut rec = hosts
        .iter()
        .find(|h| h.host == a.dest)
        .cloned()
        .unwrap_or(HostConsent {
            host: a.dest.clone(),
            ..HostConsent::default()
        });
    let mut changed = false;
    if entry.is_some() && rec.terminfo.is_none() && tty {
        rec.terminfo = Some(ask(&format!(
            "install the {ENTRY} terminfo entry into ~/.terminfo on {}?",
            a.dest
        )));
        changed = true;
    }
    if rec.env.is_none() && tty {
        rec.env = Some(ask(&format!(
            "send COLORTERM and TERM_PROGRAM to {} (the server must AcceptEnv them)?",
            a.dest
        )));
        changed = true;
    }
    if changed && let Some(p) = &path {
        hosts.retain(|h| h.host != rec.host);
        hosts.push(rec.clone());
        save_consent(p, &hosts);
    }
    let mut base = master.clone();
    if rec.env == Some(true) {
        base.extend(sshx::carry_args());
    }
    if rec.terminfo == Some(true)
        && let Some(e) = &entry
    {
        // The install runs as our child, so its askpass descends from the armed process.
        let mut c = Command::new(&ssh);
        set_askpass(&mut c);
        let ok = install_terminfo(c, &base, &a.dest, e);
        if !ok {
            eprintln!("apoth ssh: the terminfo entry could not be installed; continuing");
        }
    }
    let build = || {
        let mut c = Command::new(&ssh);
        c.args(&base);
        if a.cwd.is_some() && !a.has_command {
            c.arg("-t");
        }
        c.args(&a.ssh);
        if let (Some(d), false) = (&a.cwd, a.has_command) {
            c.arg(sshx::shell_in(d));
        }
        set_askpass(&mut c);
        c
    };
    if a.hold {
        return hold(build, &a.dest);
    }
    let e = build().exec();
    eprintln!("apoth ssh: cannot run ssh: {e}");
    ERROR
}

/// ssh's exit status when the connection failed or dropped.
const LOST: i32 = 255;
/// The user variable the window's "connection lost" notice follows (T-22 §4).
pub(crate) const LOST_VAR: &str = "apoth_ssh_lost";

/// `OSC 1337 ; SetUserVar=apoth_ssh_lost=<base64 host>` (empty host: cleared).
pub(crate) fn lost_var(host: &str) -> String {
    format!(
        "\x1b]1337;SetUserVar={LOST_VAR}={}\x07",
        crate::icat::b64(host.as_bytes())
    )
}

/// What a key typed at the "connection lost" line means.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Typed {
    Reconnect,
    Close,
    Other,
}

pub(crate) fn typed(b: u8) -> Typed {
    match b {
        b'\r' | b'\n' => Typed::Reconnect,
        0x04 => Typed::Close,
        _ => Typed::Other,
    }
}

/// Run ssh as a child until it ends for a reason other than a lost link (its status is
/// ours), offering a fresh connection each time the link drops.
fn hold(build: impl Fn() -> Command, dest: &str) -> i32 {
    use std::io::Read;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    // Ctrl-C and Ctrl-\ while ssh connects are ssh's; this parent stays (the handlers are
    // reset to the default in ssh by exec).
    let flag = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGQUIT] {
        let _ = signal_hook::flag::register(sig, Arc::clone(&flag));
    }
    loop {
        let st = match build().status() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("apoth ssh: cannot run ssh: {e}");
                return ERROR;
            }
        };
        if st.code() != Some(LOST) {
            return st.code().unwrap_or(ERROR);
        }
        let mut out = std::io::stdout().lock();
        let _ = write!(
            out,
            "{}\r\napoth ssh: the connection to {dest} was lost. Press Enter to reconnect, Ctrl-D to close.\r\n",
            lost_var(dest)
        );
        let _ = out.flush();
        drop(out);
        // Byte by byte: a crashed ssh may leave the terminal raw (Enter is then CR).
        let mut b = [0u8; 1];
        let again = loop {
            match std::io::stdin().lock().read(&mut b) {
                Ok(0) => break false,
                Ok(_) => match typed(b[0]) {
                    Typed::Reconnect => break true,
                    Typed::Close => break false,
                    Typed::Other => {}
                },
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break false,
            }
        };
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "{}", lost_var(""));
        let _ = out.flush();
        if !again {
            return LOST;
        }
    }
}

/// Ship the compiled terminfo entry through the master (stdin → `~/.terminfo`), bounded.
fn install_terminfo(mut c: Command, base: &[OsString], dest: &str, entry: &[u8]) -> bool {
    c.args(base)
        .args(["-T", "--", dest])
        .arg(sshx::terminfo_install_command(ENTRY))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = c.spawn() else {
        return false;
    };
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(entry);
    }
    let until = std::time::Instant::now() + INSTALL_WITHIN;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if std::time::Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn arguments() {
        let a = parse(&v(&["--cwd", "/srv", "-p", "22", "me@h"])).unwrap();
        assert_eq!(a.cwd.as_deref(), Some("/srv"));
        assert_eq!(a.dest, "me@h");
        assert!(!a.has_command);
        assert_eq!(a.ssh, v(&["-p", "22", "me@h"]));
        let a = parse(&v(&["h", "uptime"])).unwrap();
        assert!(a.has_command && a.cwd.is_none());
        assert!(parse(&v(&[])).is_err());
        assert!(parse(&v(&["-p", "22"])).is_err());
        assert!(parse(&v(&["--", "-oProxyCommand=x"])).is_err());
        let a = parse(&v(&["--hold", "--cwd", "/w", "me@h"])).unwrap();
        assert!(a.hold && a.cwd.as_deref() == Some("/w") && a.ssh == ["me@h"]);
        assert!(!a.has_command);
        assert!(parse(&v(&["--cwd", "/w", "--hold", "h"])).unwrap().hold);
        assert!(!parse(&v(&["h"])).unwrap().hold);
    }

    #[test]
    fn the_lost_line_and_its_variable() {
        assert_eq!(
            lost_var("me@h"),
            "\x1b]1337;SetUserVar=apoth_ssh_lost=bWVAaA==\x07"
        );
        // An empty value removes the variable (T-11 §9).
        assert_eq!(lost_var(""), "\x1b]1337;SetUserVar=apoth_ssh_lost=\x07");
        assert_eq!(typed(b'\r'), Typed::Reconnect);
        assert_eq!(typed(b'\n'), Typed::Reconnect);
        assert_eq!(typed(4), Typed::Close);
        assert_eq!(typed(b'x'), Typed::Other);
    }

    #[test]
    fn consent_file_round_trips_and_ignores_junk() {
        let hosts = vec![
            HostConsent {
                host: "me@box".into(),
                terminfo: Some(true),
                env: Some(false),
            },
            HostConsent {
                host: "other".into(),
                terminfo: None,
                env: Some(true),
            },
        ];
        let text = render_consent(&hosts);
        assert_eq!(parse_consent(&text), hosts);
        let junk = "host \"a b\" env=#true\nhost \"x\" env=yes\nnode\nhost \"x\" terminfo=#true\n";
        assert_eq!(
            parse_consent(junk),
            vec![HostConsent {
                host: "x".into(),
                terminfo: Some(true),
                env: None,
            }]
        );
        // A host the file cannot hold is not written.
        let odd = render_consent(&[HostConsent {
            host: "a\"b".into(),
            ..HostConsent::default()
        }]);
        assert!(!odd.contains("a\"b"));
    }
}
