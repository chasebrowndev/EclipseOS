// SPDX-License-Identifier: AGPL-3.0-only
//! `/proc` queries: cwd fallback, stat parsing, remote-session detection
//! (T-18 §10, NAVIGATION-28/-29/-32). Event-driven sampling is the caller's job.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::PathBuf;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, PidfdFlags, pidfd_open};

/// Walk depth bound from the foreground group's processes.
pub const MAX_WALK_DEPTH: usize = 8;
/// Process-count bound for one walk.
pub const MAX_WALK_PROCS: usize = 256;
/// `argv[0]` basenames that mean "remote" (the user list only extends it).
pub const BUILTIN_REMOTE_NAMES: &[&str] = &[
    "ssh",
    "mosh-client",
    "telnet",
    "docker",
    "podman",
    "kubectl",
    "tmux",
    "screen",
];

/// Fields of `/proc/<pid>/stat` we use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatInfo {
    /// Command name (between the parentheses; may hold spaces, `)` and non-UTF-8).
    pub comm: Vec<u8>,
    /// Process state letter.
    pub state: char,
    /// Parent pid.
    pub ppid: i32,
    /// Process group id.
    pub pgrp: i32,
    /// Session id.
    pub session: i32,
}

/// Parse a stat line. The command name is delimited by the LAST `)` because it may
/// itself contain parentheses.
pub fn parse_stat(line: &[u8]) -> Option<StatInfo> {
    let open = line.iter().position(|&b| b == b'(')?;
    let close = line.iter().rposition(|&b| b == b')')?;
    if close < open {
        return None;
    }
    let rest = String::from_utf8_lossy(&line[close + 1..]).into_owned();
    let mut f = rest.split_whitespace();
    let state = f.next()?.chars().next()?;
    let ppid = f.next()?.parse().ok()?;
    let pgrp = f.next()?.parse().ok()?;
    let session = f.next()?.parse().ok()?;
    Some(StatInfo {
        comm: line[open + 1..close].to_vec(),
        state,
        ppid,
        pgrp,
        session,
    })
}

/// `/proc/<pid>/cwd` (cwd fallback when OSC 7 is absent; the caller prefers OSC 7).
pub fn cwd(pid: i32) -> io::Result<PathBuf> {
    fs::read_link(format!("/proc/{pid}/cwd"))
}

/// Source of process facts, so the walk is testable without real processes.
pub trait ProcSource {
    /// Raw `cmdline` bytes (NUL separated). Err = unreadable or gone.
    fn cmdline(&self, pid: i32) -> io::Result<Vec<u8>>;
    /// Direct children of `pid`.
    fn children(&self, pid: i32) -> io::Result<Vec<i32>>;
    /// Every process currently in process group `pgid` (the leader may already be gone).
    fn group_members(&self, pgid: i32) -> io::Result<Vec<i32>>;
}

/// Has the process behind `pidfd` exited? (A pidfd polls readable once it has.)
fn pidfd_exited(pidfd: &impl std::os::fd::AsFd) -> bool {
    let ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut fds = [PollFd::new(pidfd, PollFlags::IN)];
    !matches!(poll(&mut fds, Some(&ts)), Ok(0))
}

/// Run `read` on `/proc/<pid>/...` with a pidfd held across it: the data is returned only if
/// the process was still alive afterwards, so it cannot belong to a recycled pid (T-18 §10:
/// "walked by pidfd").
fn with_pidfd<T>(pid: i32, read: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let gone = || io::Error::from(io::ErrorKind::NotFound);
    let pid = Pid::from_raw(pid).ok_or_else(gone)?;
    let pfd = pidfd_open(pid, PidfdFlags::empty())
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))?;
    let v = read()?;
    if pidfd_exited(&pfd) {
        return Err(gone());
    }
    Ok(v)
}

/// The real `/proc`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcFs;

impl ProcSource for ProcFs {
    fn cmdline(&self, pid: i32) -> io::Result<Vec<u8>> {
        with_pidfd(pid, || fs::read(format!("/proc/{pid}/cmdline")))
    }

    fn group_members(&self, pgid: i32) -> io::Result<Vec<i32>> {
        let mut out = Vec::new();
        for e in fs::read_dir("/proc")? {
            let Ok(e) = e else { continue };
            let Some(n) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            let Ok(line) = fs::read(format!("/proc/{n}/stat")) else {
                continue;
            };
            if parse_stat(&line).is_some_and(|s| s.pgrp == pgid && s.state != 'Z') {
                out.push(n);
            }
        }
        Ok(out)
    }

    fn children(&self, pid: i32) -> io::Result<Vec<i32>> {
        with_pidfd(pid, || self.children_raw(pid))
    }
}

impl ProcFs {
    fn children_raw(&self, pid: i32) -> io::Result<Vec<i32>> {
        let mut out = Vec::new();
        let mut complete = true;
        for t in fs::read_dir(format!("/proc/{pid}/task"))? {
            let p = t?.path().join("children");
            match fs::read_to_string(&p) {
                Ok(s) => out.extend(s.split_whitespace().filter_map(|x| x.parse::<i32>().ok())),
                Err(_) => {
                    complete = false; // kernel without CONFIG_PROC_CHILDREN
                    break;
                }
            }
        }
        if complete {
            return Ok(out);
        }
        out.clear();
        for e in fs::read_dir("/proc")? {
            let Ok(e) = e else { continue };
            let Some(n) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            let Ok(line) = fs::read(format!("/proc/{n}/stat")) else {
                continue;
            };
            if parse_stat(&line).is_some_and(|s| s.ppid == pid) {
                out.push(n);
            }
        }
        Ok(out)
    }
}

fn basename(argv0: &[u8]) -> &[u8] {
    match argv0.iter().rposition(|&b| b == b'/') {
        Some(i) => &argv0[i + 1..],
        None => argv0,
    }
}

fn is_remote_name(argv0: &[u8], user: &[String]) -> bool {
    let b = basename(argv0);
    BUILTIN_REMOTE_NAMES.iter().any(|n| n.as_bytes() == b) || user.iter().any(|n| n.as_bytes() == b)
}

/// Does the process tree rooted at the foreground process group `pgid` contain a remote
/// session? Seeded with every process of the group (not only its leader, which may have
/// exited or may be a pipeline's first stage) and walked through their descendants,
/// breadth-first, depth <= 8, <= 256 processes (T-18 §10). An unreadable cmdline, a hit
/// bound, an unreadable or empty group all count as a match (every test only tightens).
/// An empty cmdline (zombie) is not a match.
pub fn is_remote<S: ProcSource>(src: &S, pgid: i32, user: &[String]) -> bool {
    let seeds = match src.group_members(pgid) {
        Ok(v) if !v.is_empty() => v,
        _ => return true,
    };
    let mut q: VecDeque<(i32, usize)> = seeds.into_iter().map(|p| (p, 0)).collect();
    let mut seen: HashSet<i32> = HashSet::new();
    while let Some((pid, depth)) = q.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        if seen.len() > MAX_WALK_PROCS {
            return true;
        }
        match src.cmdline(pid) {
            Err(_) => return true,
            Ok(c) => {
                let argv0 = c.split(|&b| b == 0).next().unwrap_or(&[]);
                if !argv0.is_empty() && is_remote_name(argv0, user) {
                    return true;
                }
            }
        }
        let kids = src.children(pid).unwrap_or_default();
        if !kids.is_empty() && depth >= MAX_WALK_DEPTH {
            return true;
        }
        q.extend(kids.into_iter().map(|k| (k, depth + 1)));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn stat_parsed_from_last_paren() {
        let l = b"123 (we) ird ) name) S 45 67 89 34816 67 4194560 1 0";
        let s = parse_stat(l).unwrap();
        assert_eq!(s.comm, b"we) ird ) name");
        assert_eq!((s.state, s.ppid, s.pgrp, s.session), ('S', 45, 67, 89));
        assert!(parse_stat(b"garbage").is_none());
        assert!(parse_stat(b"1 (x) S 2").is_none());
    }

    #[test]
    fn stat_of_self_parses() {
        let l = fs::read("/proc/self/stat").unwrap();
        let s = parse_stat(&l).unwrap();
        assert!(s.ppid > 0 && s.pgrp > 0);
    }

    #[derive(Default)]
    struct Fake {
        cmd: HashMap<i32, Option<&'static str>>,
        kids: HashMap<i32, Vec<i32>>,
        groups: HashMap<i32, Vec<i32>>,
    }
    impl Fake {
        fn p(mut self, pid: i32, cmd: Option<&'static str>, kids: &[i32]) -> Self {
            self.cmd.insert(pid, cmd);
            self.kids.insert(pid, kids.to_vec());
            self
        }
    }
    impl ProcSource for Fake {
        fn cmdline(&self, pid: i32) -> io::Result<Vec<u8>> {
            match self.cmd.get(&pid) {
                Some(Some(s)) => Ok(s.replace(' ', "\0").into_bytes()),
                _ => Err(io::ErrorKind::PermissionDenied.into()),
            }
        }
        fn children(&self, pid: i32) -> io::Result<Vec<i32>> {
            Ok(self.kids.get(&pid).cloned().unwrap_or_default())
        }
        fn group_members(&self, pgid: i32) -> io::Result<Vec<i32>> {
            // Default: the group is just its leader.
            Ok(self
                .groups
                .get(&pgid)
                .cloned()
                .unwrap_or_else(|| vec![pgid]))
        }
    }

    #[test]
    fn local_shell_is_not_remote() {
        let f = Fake::default()
            .p(1, Some("-zsh"), &[2])
            .p(2, Some("vim file"), &[]);
        assert!(!is_remote(&f, 1, &[]));
    }

    #[test]
    fn ssh_under_tmux_sudo_nested_docker_are_remote() {
        let f = Fake::default()
            .p(1, Some("zsh"), &[2])
            .p(2, Some("/usr/bin/tmux attach"), &[]);
        assert!(is_remote(&f, 1, &[]));
        let f = Fake::default()
            .p(1, Some("zsh"), &[2])
            .p(2, Some("sudo ssh h"), &[3])
            .p(3, Some("/usr/bin/ssh h"), &[]);
        assert!(is_remote(&f, 1, &[]));
        let f = Fake::default()
            .p(1, Some("zsh"), &[2])
            .p(2, Some("docker exec -it c sh"), &[]);
        assert!(is_remote(&f, 1, &[]));
        let f = Fake::default()
            .p(1, Some("zsh"), &[2])
            .p(2, Some("bash"), &[3])
            .p(3, Some("bash -c x"), &[4])
            .p(4, Some("ssh h"), &[]);
        assert!(is_remote(&f, 1, &[]));
    }

    #[test]
    fn walk_is_seeded_from_the_whole_foreground_group() {
        // Group 10: leader 10 is a plain shell stage, member 11 is ssh (pipeline).
        let mut f = Fake::default()
            .p(10, Some("sh"), &[])
            .p(11, Some("ssh h"), &[])
            .p(1, Some("zsh"), &[10, 11]);
        f.groups.insert(10, vec![10, 11]);
        assert!(is_remote(&f, 10, &[]));
        // The shell's own (parent) ssh sibling is outside the foreground group's tree.
        let mut f = Fake::default()
            .p(10, Some("vim"), &[])
            .p(1, Some("ssh h"), &[10]);
        f.groups.insert(10, vec![10]);
        assert!(!is_remote(&f, 10, &[]));
        // Leader gone and group empty/unreadable: tighten.
        f.groups.insert(20, vec![]);
        assert!(is_remote(&f, 20, &[]));
    }

    #[test]
    fn unreadable_child_is_remote() {
        let f = Fake::default().p(1, Some("zsh"), &[2]).p(2, None, &[]);
        assert!(is_remote(&f, 1, &[]));
    }

    #[test]
    fn user_list_extends_builtin() {
        let f = Fake::default()
            .p(1, Some("zsh"), &[2])
            .p(2, Some("myjump host"), &[]);
        assert!(!is_remote(&f, 1, &[]));
        assert!(is_remote(&f, 1, &["myjump".to_string()]));
    }

    #[test]
    fn bounds_count_as_match() {
        // chain deeper than 8
        let mut f = Fake::default();
        for i in 1..=12 {
            let kids = [i + 1];
            f = f.p(i, Some("sh"), if i < 12 { &kids[..] } else { &[] });
        }
        assert!(is_remote(&f, 1, &[]));
        // wide: > 256 children
        let kids: Vec<i32> = (2..400).collect();
        let mut f = Fake::default().p(1, Some("sh"), &kids);
        for k in kids {
            f = f.p(k, Some("sleep 1"), &[]);
        }
        assert!(is_remote(&f, 1, &[]));
    }
}
