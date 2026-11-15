// SPDX-License-Identifier: AGPL-3.0-only
//! The system `ssh` as transport (T-22 §2, §4, §5; REMOTE-08, -09, -10). No ssh protocol
//! here (REMOTE-28): argument building, the private `ControlMaster` directory, the stale
//! master probe and remote-command quoting, shared by `apoth ssh` and the remote mux link.
//!
//! `ControlPath` is `$XDG_RUNTIME_DIR/apotheosis/ssh/%C`. ssh binds the master socket under
//! a temporary name (the path plus a 17-byte suffix) before renaming it, and `%C` expands to
//! 40 hex digits, so a long runtime directory overflows `sun_path` (108 bytes). Then a short
//! per-uid symlink `/tmp/apoth-ssh-<uid>` to the directory is used instead (REMOTE-09); one
//! that exists and is not our symlink to our directory means no shared master at all (plain
//! ssh semantics), never someone else's directory.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `sun_path` on Linux.
pub const SUN_PATH_MAX: usize = 108;
/// `%C`: a SHA-1 in hex.
const HASH_LEN: usize = 40;
/// ssh's temporary suffix while it binds the master socket.
const TEMP_SUFFIX: usize = 17;
/// `ControlPersist` (T-22 §2 names no value; REMOTE-08 says `yes` and "tear down on exit",
/// which nothing here can promise for a master another tab may share: reported).
pub const CONTROL_PERSIST: &str = "600";
/// `ServerAliveInterval` / `ServerAliveCountMax` (REMOTE-08).
pub const SERVER_ALIVE: (&str, &str) = ("60", "5");
/// The stale-master probe's bound (T-22 §2).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Environment names the §2 carry passes on (when consented for the host).
pub const CARRY_ENV: &[&str] = &["COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"];

/// `<runtime>/apotheosis/ssh`.
#[must_use]
pub fn control_dir(runtime: &Path) -> PathBuf {
    runtime.join("apotheosis").join("ssh")
}

/// The master socket ssh would bind under `dir` fits `sun_path`.
#[must_use]
pub fn control_path_fits(dir: &Path) -> bool {
    dir.as_os_str().len() + 1 + HASH_LEN + TEMP_SUFFIX < SUN_PATH_MAX
}

/// The short-path link for `uid`.
#[must_use]
pub fn short_link(uid: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/apoth-ssh-{uid}"))
}

fn private_dir(dir: &Path, uid: u32) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let md = fs::symlink_metadata(dir)?;
    if !md.is_dir() || md.uid() != uid || md.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "the ssh control directory is not a private directory of ours",
        ));
    }
    Ok(())
}

/// Create `<runtime>/apotheosis/ssh` (`0700`, ours) and return the directory `ControlPath`
/// names: itself, or the short link when the path is too long. `Err` = no shared master.
pub fn prepare_control_dir(runtime: &Path, uid: u32) -> io::Result<PathBuf> {
    if !runtime.is_absolute() {
        return Err(io::Error::other("XDG_RUNTIME_DIR is not absolute"));
    }
    let base = runtime.join("apotheosis");
    private_dir(&base, uid)?;
    let dir = control_dir(runtime);
    private_dir(&dir, uid)?;
    if control_path_fits(&dir) {
        return Ok(dir);
    }
    let link = short_link(uid);
    match fs::symlink_metadata(&link) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::os::unix::fs::symlink(&dir, &link)?;
        }
        Err(e) => return Err(e),
    }
    let md = fs::symlink_metadata(&link)?;
    if !md.file_type().is_symlink() || md.uid() != uid || fs::read_link(&link)? != dir {
        return Err(io::Error::other("the short ssh control link is not ours"));
    }
    Ok(link)
}

/// The options every ssh we run gets: the private master when `dir` is known, keep-alives
/// always (REMOTE-08).
#[must_use]
pub fn master_args(dir: Option<&Path>) -> Vec<OsString> {
    let mut v: Vec<OsString> = Vec::new();
    let mut o = |s: OsString| {
        v.push("-o".into());
        v.push(s);
    };
    if let Some(d) = dir {
        o("ControlMaster=auto".into());
        let mut p = OsString::from("ControlPath=");
        p.push(d.join("%C").as_os_str());
        o(p);
        o(format!("ControlPersist={CONTROL_PERSIST}").into());
    }
    o(format!("ServerAliveInterval={}", SERVER_ALIVE.0).into());
    o(format!("ServerAliveCountMax={}", SERVER_ALIVE.1).into());
    o("TCPKeepAlive=no".into());
    v
}

/// `-o SendEnv=…` for the §2 environment carry.
#[must_use]
pub fn carry_args() -> Vec<OsString> {
    CARRY_ENV
        .iter()
        .flat_map(|k| ["-o".into(), OsString::from(format!("SendEnv={k}"))])
        .collect()
}

/// What `ssh -O check` said about the shared master.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// A master answers.
    Alive,
    /// No master (ssh will start one).
    Absent,
    /// The probe did not finish in time: a stale master (it was told to exit).
    Stale,
}

fn bounded(mut c: Command, t: Duration) -> Option<std::process::ExitStatus> {
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let until = Instant::now() + t;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                // Our own child, by its handle.
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Probe the shared master for `dest` before reuse (T-22 §2): `ssh -O check`, 5 s. A probe
/// that hangs means a master whose connection is gone; it is told to exit (bounded) so the
/// next ssh starts a fresh one.
pub fn probe_master(ssh: &OsStr, master: &[OsString], dest: &str, timeout: Duration) -> Probe {
    let mut c = Command::new(ssh);
    c.args(master).args(["-O", "check", "--", dest]);
    match bounded(c, timeout) {
        Some(s) if s.success() => Probe::Alive,
        Some(_) => Probe::Absent,
        None => {
            let mut c = Command::new(ssh);
            c.args(master).args(["-O", "exit", "--", dest]);
            let _ = bounded(c, Duration::from_secs(2));
            Probe::Stale
        }
    }
}

/// POSIX single-quoting for a remote command word.
#[must_use]
pub fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// The remote command for a shell in `cwd` (OSC 7 inheritance, T-22 §4): `cd` (failure
/// ignored) then the user's login shell.
#[must_use]
pub fn shell_in(cwd: &str) -> String {
    format!("cd {} 2>/dev/null; exec \"$SHELL\" -l", sh_quote(cwd))
}

/// `ssh` options that take an argument (OpenSSH 9).
const ARG_OPTS: &[u8] = b"BbcDEeFIiJLlmOoPpQRSWw";

/// Index of the destination in ssh's argument list: the first word that is not an option or
/// an option's argument. `None` when there is none.
#[must_use]
pub fn destination_index(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_bytes();
        if a == b"--" {
            return (i + 1 < args.len()).then_some(i + 1);
        }
        if a.len() < 2 || a[0] != b'-' {
            return Some(i);
        }
        let mut takes = false;
        for (k, c) in a[1..].iter().enumerate() {
            if ARG_OPTS.contains(c) {
                // `-p22` carries its value; `-p 22` takes the next word.
                takes = k + 2 == a.len();
                break;
            }
        }
        i += if takes { 2 } else { 1 };
    }
    None
}

/// A destination we pass to ssh: 1-255 bytes, no leading `-` (it would be an option), no
/// whitespace or control characters.
#[must_use]
pub fn valid_destination(s: &str) -> bool {
    (1..=255).contains(&s.len())
        && !s.starts_with('-')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The compiled `xterm-apotheosis` entry from the local terminfo database (`infocmp`-free:
/// the file itself is what `apoth ssh` sends), bounded.
#[must_use]
pub fn compiled_entry(dirs: &[PathBuf], name: &str) -> Option<Vec<u8>> {
    let first = name.bytes().next()?;
    for d in dirs {
        for sub in [
            d.join(char::from(first).to_string()).join(name),
            d.join(format!("{first:02x}")).join(name),
        ] {
            if let Ok(md) = fs::metadata(&sub)
                && md.is_file()
                && md.len() <= 64 * 1024
                && let Ok(b) = fs::read(&sub)
            {
                return Some(b);
            }
        }
    }
    None
}

/// The remote command that installs a compiled entry read from stdin into `~/.terminfo`
/// (both directory layouts), atomically per file.
#[must_use]
pub fn terminfo_install_command(name: &str) -> String {
    let first = name.chars().next().unwrap_or('x');
    let hex = format!("{:02x}", u32::from(first));
    let q = sh_quote(name);
    let script = format!(
        "umask 077; d=\"$HOME/.terminfo\"; mkdir -p \"$d/{first}\" \"$d/{hex}\" && \
         cat > \"$d/{first}/.{name}.tmp\" && mv -f \"$d/{first}/.{name}.tmp\" \"$d/{first}/\"{q} && \
         cp -f \"$d/{first}/\"{q} \"$d/{hex}/.{name}.tmp\" && mv -f \"$d/{hex}/.{name}.tmp\" \"$d/{hex}/\"{q}"
    );
    format!("sh -c {}", sh_quote(&script))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn finds_the_destination() {
        assert_eq!(destination_index(&s(&["host"])), Some(0));
        assert_eq!(destination_index(&s(&["-p", "22", "me@h", "ls"])), Some(2));
        assert_eq!(destination_index(&s(&["-p22", "h"])), Some(1));
        assert_eq!(destination_index(&s(&["-vvt", "-A", "h"])), Some(2));
        assert_eq!(destination_index(&s(&["-tl", "me", "h"])), Some(2));
        assert_eq!(
            destination_index(&s(&["-o", "X=y", "-i", "k", "h", "-x"])),
            Some(4)
        );
        assert_eq!(destination_index(&s(&["--", "-weird"])), Some(1));
        assert_eq!(destination_index(&s(&["-p"])), None);
        assert_eq!(destination_index(&s(&[])), None);
    }

    #[test]
    fn destinations_and_quoting() {
        assert!(valid_destination("me@host.example"));
        assert!(valid_destination("box"));
        for bad in ["", "-oProxyCommand=x", "a b", "a\nb", &"h".repeat(256)] {
            assert!(!valid_destination(bad), "{bad:?}");
        }
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
        assert_eq!(
            shell_in("/srv/my dir"),
            "cd '/srv/my dir' 2>/dev/null; exec \"$SHELL\" -l"
        );
        let c = terminfo_install_command("xterm-apotheosis");
        assert!(c.starts_with("sh -c '"));
        assert!(c.contains("78") && c.contains(".terminfo"));
    }

    #[test]
    fn control_path_and_options() {
        assert!(control_path_fits(Path::new(
            "/run/user/1000/apotheosis/ssh"
        )));
        let long = PathBuf::from(format!("/{}", "r".repeat(60))).join("apotheosis/ssh");
        assert!(!control_path_fits(&long));
        assert!(control_path_fits(&short_link(4_000_000_000)));
        let a = master_args(Some(Path::new("/run/user/1/apotheosis/ssh")));
        let a: Vec<String> = a.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(a.contains(&"ControlMaster=auto".to_owned()));
        assert!(a.contains(&"ControlPath=/run/user/1/apotheosis/ssh/%C".to_owned()));
        assert!(a.contains(&"ServerAliveInterval=60".to_owned()));
        let plain = master_args(None);
        assert!(
            !plain
                .iter()
                .any(|x| x.to_string_lossy().starts_with("Control"))
        );
        assert_eq!(carry_args().len(), 2 * CARRY_ENV.len());
    }

    #[test]
    fn control_dir_is_private_and_links_only_to_ours() {
        let uid = rustix::process::getuid().as_raw();
        let rt = std::env::temp_dir().join(format!("apssh{}", std::process::id()));
        let _ = fs::remove_dir_all(&rt);
        fs::create_dir_all(&rt).unwrap();
        let d = prepare_control_dir(&rt, uid).unwrap();
        assert_eq!(d, control_dir(&rt));
        let md = fs::metadata(&d).unwrap();
        assert_eq!(md.mode() & 0o777, 0o700);
        // Another uid's directory is refused.
        assert!(prepare_control_dir(&rt, uid.wrapping_add(1)).is_err());
        assert!(prepare_control_dir(Path::new("rel"), uid).is_err());
        fs::remove_dir_all(&rt).unwrap();
    }

    #[test]
    fn finds_a_compiled_entry_in_either_layout() {
        let d = std::env::temp_dir().join(format!("apti{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("78")).unwrap();
        fs::write(d.join("78/xterm-apotheosis"), b"\x1a\x01entry").unwrap();
        assert_eq!(
            compiled_entry(std::slice::from_ref(&d), "xterm-apotheosis").as_deref(),
            Some(&b"\x1a\x01entry"[..])
        );
        assert!(compiled_entry(std::slice::from_ref(&d), "nope").is_none());
        fs::remove_dir_all(&d).unwrap();
    }
}
