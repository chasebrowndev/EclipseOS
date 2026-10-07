// SPDX-License-Identifier: AGPL-3.0-only
//! The `policyd` link, agentd role (C2, A-01 §5).
//!
//! Dials `policyd.sock` (SEQPACKET), consumes the key offer, then decodes
//! [`FromPolicyd`] messages for the core and sends whatever the core queues
//! (`ToPolicyd` messages and audit emissions). Redials every second. agentd
//! never verifies the key or any grant: abyss does, and this process holds no
//! authority beyond what policyd hands it.

use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

use ec_policy_eval::link::{self as wire, FromPolicyd};
use rustix::io::Errno;
use rustix::net::{self, RecvFlags, SendFlags, SocketAddrUnix, SocketFlags};

use crate::core::Msg;

fn dial(path: &std::path::Path) -> Option<OwnedFd> {
    let addr = SocketAddrUnix::new(path).ok()?;
    let fd = net::socket_with(
        net::AddressFamily::UNIX,
        net::SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .ok()?;
    net::connect(&fd, &addr).ok()?;
    net::sockopt::set_socket_timeout(&fd, net::sockopt::Timeout::Recv, Some(Duration::from_millis(30)))
        .ok()?;
    Some(fd)
}

pub fn spawn(path: PathBuf, tx: Sender<Msg>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if let Some(fd) = dial(&path) {
                session(&fd, &tx, &stop);
                let _ = tx.send(Msg::LinkDown);
            }
            for _ in 0..10 {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    });
}

fn session(fd: &OwnedFd, tx: &Sender<Msg>, stop: &AtomicBool) {
    let mut buf = vec![0u8; wire::MAX_MESSAGE + 1024];
    let (otx, orx) = channel::<Vec<u8>>();
    let mut up = false;
    while !stop.load(Ordering::Relaxed) {
        match net::recv(fd, &mut buf[..], RecvFlags::empty()) {
            Ok((0, _)) => return,
            Ok((n, _)) => {
                let m = &buf[..n];
                if wire::is_message(m) {
                    // Unknown or unwanted messages (tables, defer answers) are
                    // not ours: ignore them rather than drop the link.
                    if let Ok(msg) = FromPolicyd::decode(m) {
                        if tx.send(Msg::Link(msg)).is_err() {
                            return;
                        }
                    }
                } else if !up {
                    // The key offer. agentd does not use the key.
                    up = true;
                    if tx.send(Msg::LinkUp(otx.clone())).is_err() {
                        return;
                    }
                }
            }
            Err(Errno::AGAIN) | Err(Errno::INTR) => {}
            Err(_) => return,
        }
        while let Ok(b) = orx.try_recv() {
            if net::send(fd, &b, SendFlags::NOSIGNAL).is_err() {
                return;
            }
        }
    }
}
