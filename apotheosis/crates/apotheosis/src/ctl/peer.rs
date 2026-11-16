// SPDX-License-Identifier: AGPL-3.0-only
//! Peer identity (T-12 §1), taken once at connect: `SO_PEERCRED` (the uid must be ours),
//! then a pidfd. `SO_PEERPIDFD` has no safe binding in our `rustix`, so this is the spec's
//! fallback: `pidfd_open` plus a start-time check (the process must have started before we
//! accepted the connection, so a recycled pid is refused). Every `/proc` read is followed by
//! a pidfd liveness check. Nothing read here is logged but the pid.

use std::fs;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use apoth_pty::{BUILTIN_REMOTE_NAMES, StatInfo, parse_stat};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, PidfdFlags, getuid, pidfd_open};

use super::caps::PeerFacts;

/// Ancestors walked looking for one of our panes.
const MAX_ANCESTRY: usize = 64;

/// A connected peer.
#[derive(Debug)]
pub(crate) struct Peer {
    /// Process id (for logs and the descendant check).
    pub(crate) pid: i32,
    /// Held for the connection's life: its readability means the peer exited.
    pub(crate) pidfd: OwnedFd,
    /// `/proc/<pid>/exe`.
    pub(crate) exe: Option<PathBuf>,
    /// What the capability table reads.
    pub(crate) facts: PeerFacts,
}

/// Why a peer was refused.
#[derive(Debug)]
pub(crate) enum Refused {
    /// Another uid.
    Uid,
    /// The process is gone, or its pid was recycled.
    Gone,
    /// `/proc` or the socket could not be read.
    Io(io::Error),
}

impl From<io::Error> for Refused {
    fn from(e: io::Error) -> Refused {
        Refused::Io(e)
    }
}

/// Field 22 of `/proc/<pid>/stat`: start time in clock ticks after boot.
#[must_use]
pub(crate) fn parse_starttime(line: &[u8]) -> Option<u64> {
    let close = line.iter().rposition(|&b| b == b')')?;
    let rest = std::str::from_utf8(&line[close + 1..]).ok()?;
    // Fields after the command name start at field 3 (state); starttime is field 22.
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Runs under the agents slice (`/proc/<pid>/cgroup`).
#[must_use]
pub(crate) fn cgroup_is_agent(cgroup: &str) -> bool {
    cgroup
        .lines()
        .filter_map(|l| l.splitn(3, ':').nth(2))
        .any(|path| path.split('/').any(|seg| seg == "agents.slice"))
}

/// Walk from `pid` up its parents: the first ancestor (or the peer itself) that is one of
/// our pane children names the pane; `remote` is set when a remote-session program sits on
/// that path. `stat` reads a process (None = gone or unreadable).
pub(crate) fn ancestry(
    pid: i32,
    panes: &[(u64, i32)],
    mut stat: impl FnMut(i32) -> Option<StatInfo>,
) -> (Option<u64>, bool) {
    let mut remote = false;
    let mut cur = pid;
    for _ in 0..MAX_ANCESTRY {
        if let Some(&(pane, _)) = panes.iter().find(|(_, p)| *p == cur) {
            return (Some(pane), remote);
        }
        let Some(st) = stat(cur) else { break };
        let comm = String::from_utf8_lossy(&st.comm);
        if BUILTIN_REMOTE_NAMES.contains(&comm.as_ref()) {
            remote = true;
        }
        if st.ppid <= 1 {
            break;
        }
        cur = st.ppid;
    }
    (None, false)
}

fn alive(pidfd: &impl AsFd) -> bool {
    let ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut fds = [PollFd::new(pidfd, PollFlags::IN)];
    matches!(poll(&mut fds, Some(&ts)), Ok(0))
}

fn boot_ticks() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
    let hz = rustix::param::clock_ticks_per_second().max(1);
    (t.tv_sec.max(0) as u64)
        .saturating_mul(hz)
        .saturating_add((t.tv_nsec.max(0) as u64).saturating_mul(hz) / 1_000_000_000)
}

/// Identify the peer of an accepted connection. `panes` are our panes' child pids;
/// `interpreters` extends the built-in interpreter list (`socket.interpreters`).
pub(crate) fn identify(
    stream: &UnixStream,
    panes: &[(u64, i32)],
    interpreters: &[String],
) -> Result<Peer, Refused> {
    let accepted = boot_ticks();
    let cred = rustix::net::sockopt::socket_peercred(stream)
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))?;
    if cred.uid != getuid() {
        return Err(Refused::Uid);
    }
    let pid = cred.pid.as_raw_nonzero().get();
    let pidfd = pidfd_open(
        Pid::from_raw(pid).ok_or(Refused::Gone)?,
        PidfdFlags::empty(),
    )
    .map_err(|_| Refused::Gone)?;
    let stat = fs::read(format!("/proc/{pid}/stat"))?;
    // +1 tick: the start time is rounded down to a tick.
    match parse_starttime(&stat) {
        Some(st) if st <= accepted + 1 => {}
        _ => return Err(Refused::Gone),
    }
    let exe = fs::read_link(format!("/proc/{pid}/exe")).ok();
    let agent = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map(|c| cgroup_is_agent(&c))
        .unwrap_or(false);
    let (pane, remote) = ancestry(pid, panes, |p| {
        fs::read(format!("/proc/{p}/stat"))
            .ok()
            .and_then(|l| parse_stat(&l))
    });
    if !alive(&pidfd) {
        return Err(Refused::Gone);
    }
    let interpreter = exe
        .as_deref()
        .and_then(|p| p.to_str())
        .is_some_and(|p| apoth_config::lang::is_interpreter(p, interpreters));
    Ok(Peer {
        pid,
        pidfd,
        exe,
        facts: PeerFacts {
            pane,
            remote,
            agent,
            interpreter,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(comm: &str, ppid: i32) -> StatInfo {
        StatInfo {
            comm: comm.as_bytes().to_vec(),
            state: 'S',
            ppid,
            pgrp: 0,
            session: 0,
        }
    }

    #[test]
    fn starttime_is_field_22_even_with_parens_in_the_name() {
        let line = b"42 (a) b (c)) S 1 42 42 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 98765 0 0";
        assert_eq!(parse_starttime(line), Some(98765));
        assert_eq!(parse_starttime(b"1 (x) S 0"), None);
    }

    #[test]
    fn agent_cgroup_detection() {
        assert!(cgroup_is_agent(
            "0::/user.slice/user-1000.slice/user@1000.service/agents.slice/run-1.scope\n"
        ));
        assert!(!cgroup_is_agent(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/foot.scope\n"
        ));
        assert!(!cgroup_is_agent("0::/my-agents.slice.d/x\n"));
    }

    #[test]
    fn ancestry_finds_the_pane_and_remote_hops() {
        let panes = [(5u64, 100)];
        // tool(300) <- zsh(200) <- shell(100) = pane 5.
        let tree = |p| match p {
            300 => Some(st("tool", 200)),
            200 => Some(st("zsh", 100)),
            100 => Some(st("zsh", 1)),
            _ => None,
        };
        assert_eq!(ancestry(300, &panes, tree), (Some(5), false));
        // A client behind ssh inside the pane is remote.
        let tree = |p| match p {
            300 => Some(st("apoth", 250)),
            250 => Some(st("ssh", 100)),
            _ => None,
        };
        assert_eq!(ancestry(300, &panes, tree), (Some(5), true));
        // Not ours at all.
        let tree = |p| match p {
            300 => Some(st("x", 1)),
            _ => None,
        };
        assert_eq!(ancestry(300, &panes, tree), (None, false));
    }
}
