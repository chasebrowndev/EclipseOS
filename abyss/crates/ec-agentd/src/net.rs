// SPDX-License-Identifier: AGPL-3.0-only
//! Unix-socket plumbing for the console and MCP servers: peer checks, an
//! accept loop and one reader and one writer thread per connection. All state
//! stays in the core (`core.rs`); these threads only move lines.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

use crate::core::{Msg, Surface};

/// The longest request line either socket accepts.
pub const MAX_LINE: usize = 64 * 1024;

/// The same for a per-task MCP socket: an `inference.complete` carries a whole
/// conversation, and the router accepts frames up to this size.
pub const MCP_MAX_LINE: usize = ec_inference_wire::MAX_FRAME;

pub type PeerPolicy = Arc<dyn Fn(&UnixStream) -> bool + Send + Sync>;

/// `(uid, pid)` of the connected peer.
pub fn peer_cred(s: &UnixStream) -> Option<(u32, i32)> {
    let c = rustix::net::sockopt::socket_peercred(s).ok()?;
    Some((c.uid.as_raw(), c.pid.as_raw_nonzero().get()))
}

/// Whether a `/proc/<pid>/cgroup` body places the process under
/// `agents.slice`. cgroup v2 has one `0::` line, v1 has many: any line
/// counts. An empty body (unreadable) counts as yes.
pub fn under_agents(cgroup: &str) -> bool {
    if cgroup.trim().is_empty() {
        return true;
    }
    cgroup.lines().any(|l| {
        let path = l.rsplit(':').next().unwrap_or("");
        path.split('/').any(|c| c == "agents.slice")
    })
}

/// The console rule (A-08 §7): our uid, and not inside an agent scope.
pub fn console_peer_ok(uid: u32, ours: u32, cgroup: &str) -> bool {
    uid == ours && !under_agents(cgroup)
}

pub fn our_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

pub fn console_policy() -> PeerPolicy {
    Arc::new(|s| {
        let Some((uid, pid)) = peer_cred(s) else {
            return false;
        };
        let cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
        console_peer_ok(uid, our_uid(), &cg)
    })
}

/// MCP sockets are reached from inside the agent's scope, so only the uid is
/// checked; the socket's path is in the task's own 0700 directory and is the
/// only socket bound into its sandbox.
pub fn mcp_policy() -> PeerPolicy {
    Arc::new(|s| peer_cred(s).is_some_and(|(uid, _)| uid == our_uid()))
}

/// Accepts on `listener` until `stop`, handing each admitted connection to the
/// core. The listener is polled, not blocked on, so `stop` takes effect.
pub fn serve(
    listener: UnixListener,
    surface: Surface,
    policy: PeerPolicy,
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
    ids: Arc<AtomicU64>,
) {
    let _ = listener.set_nonblocking(true);
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((s, _)) => {
                    let _ = s.set_nonblocking(false);
                    if !policy(&s) {
                        let _ = s.shutdown(std::net::Shutdown::Both);
                        continue;
                    }
                    if !start_conn(s, &surface, &tx, &ids) {
                        return;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(15));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    });
}

fn start_conn(s: UnixStream, surface: &Surface, tx: &Sender<Msg>, ids: &AtomicU64) -> bool {
    let (Ok(mut w), Ok(mut r), Ok(handle)) = (s.try_clone(), s.try_clone(), s.try_clone()) else {
        return true;
    };
    drop(s);
    let conn = ids.fetch_add(1, Ordering::Relaxed);
    let (wtx, wrx) = channel::<String>();
    std::thread::spawn(move || {
        for line in wrx {
            if w.write_all(line.as_bytes()).is_err() || w.write_all(b"\n").is_err() {
                break;
            }
        }
    });
    // Open goes first so the core knows the connection before any line.
    if tx
        .send(Msg::Open {
            conn,
            surface: surface.clone(),
            tx: wtx,
            stream: handle,
        })
        .is_err()
    {
        return false;
    }
    let tx = tx.clone();
    let max_line = match surface {
        Surface::Console => MAX_LINE,
        Surface::Mcp(_) => MCP_MAX_LINE,
    };
    std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        'outer: loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(p) = buf.iter().position(|b| *b == b'\n') {
                        // A line over the cap is refused even when its newline
                        // arrived in the same read as the overflow.
                        if p > max_line {
                            break 'outer;
                        }
                        let line: Vec<u8> = buf.drain(..=p).collect();
                        let s = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
                        if !s.trim().is_empty() && tx.send(Msg::Line { conn, line: s }).is_err() {
                            return;
                        }
                    }
                    if buf.len() > max_line {
                        break 'outer;
                    }
                }
            }
        }
        let _ = tx.send(Msg::Closed { conn });
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_slice_is_refused() {
        assert!(under_agents(
            "0::/user.slice/agents.slice/agent-x.slice/launch-1.scope\n"
        ));
        assert!(under_agents("1:name=systemd:/agents.slice/a\n0::/\n"));
        assert!(under_agents(""));
        assert!(!under_agents("0::/user.slice/user-1000.slice/session-3.scope\n"));
        assert!(!under_agents("0::/\n"));
        // A lookalike is not the slice.
        assert!(!under_agents("0::/user.slice/my-agents.slice.d/x\n"));
    }

    #[test]
    fn console_needs_our_uid_and_no_agent_scope() {
        assert!(console_peer_ok(1000, 1000, "0::/user.slice/s.scope"));
        assert!(!console_peer_ok(1001, 1000, "0::/user.slice/s.scope"));
        assert!(!console_peer_ok(1000, 1000, "0::/agents.slice/x"));
    }
}
