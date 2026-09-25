// SPDX-License-Identifier: AGPL-3.0-only
//! The wipe confirmation (D-07 §6, COMP-10 §3.10, ADR 0061): before
//! repartitioning, the helper asks the compositor's trusted UI to show the
//! disk's model, size and by-id name and to return allow or deny from the human
//! seat.
//!
//! The helper is root and the compositor is the caller's own process, so the
//! helper authenticates the *compositor*: the peer of the socket must be the
//! calling user's `abyss`, by pid, uid and `/proc/<pid>/exe`. A same-uid fake
//! listener at the socket path fails that check. The live medium sets
//! `kernel.yama.ptrace_scope=1`, so it cannot become the real one either.
//! Anything but one exact `allow` line is a deny.

use crate::error::{io, Error, Result};
use eclipse_setup_plan::Disk;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub trait Confirm {
    /// `Ok(true)` only on an explicit allow from the human seat. Any `Err` is
    /// treated as a deny by the caller: fail closed.
    fn confirm_erase(&self, disk: &Disk) -> Result<bool>;
}

/// The compositor's destructive-action prompt (COMP-10 §3.10).
pub struct SocketConfirm {
    pub socket: PathBuf,
    /// Where the compositor's `/proc/<pid>` lives; `/proc` outside tests.
    pub proc: PathBuf,
    /// The binary the peer must be running.
    pub exe: PathBuf,
    /// The calling user: the socket's owner and the compositor's uid.
    pub uid: u32,
}

/// The compositor gives up after 120 s and denies; wait a little longer.
const REPLY_WAIT: Duration = Duration::from_secs(130);
const MAX_REPLY: u64 = 256;

impl SocketConfirm {
    /// `uid` is `PKEXEC_UID`: it only says where to look and who to expect.
    /// The peer check, not the path, is what is trusted.
    pub fn for_caller(uid: u32) -> Self {
        SocketConfirm {
            socket: format!("/run/user/{uid}/eclipse/trusted.sock").into(),
            proc: "/proc".into(),
            exe: "/usr/bin/abyss".into(),
            uid,
        }
    }

    fn peer_is_the_compositor(&self, stream: &UnixStream) -> bool {
        let Ok(cred) = rustix::net::sockopt::socket_peercred(stream) else {
            return false;
        };
        let pid = cred.pid.as_raw_nonzero().get();
        if cred.uid.as_raw() != self.uid || pid <= 0 {
            return false;
        }
        // A replaced binary reads back with " (deleted)" and fails here too.
        std::fs::read_link(self.proc.join(pid.to_string()).join("exe")).is_ok_and(|exe| exe == self.exe)
    }
}

impl Confirm for SocketConfirm {
    fn confirm_erase(&self, disk: &Disk) -> Result<bool> {
        let mut s = UnixStream::connect(&self.socket).map_err(io("connect trusted socket"))?;
        if !self.peer_is_the_compositor(&s) {
            return Err(Error::Refused("trusted socket peer is not the compositor"));
        }
        s.set_read_timeout(Some(REPLY_WAIT)).map_err(io("timeout"))?;
        let req = serde_json::json!({
            "action": "erase-disk",
            "disk": disk.by_id,
            "model": disk.model,
            "size_bytes": disk.size_bytes,
        });
        writeln!(s, "{req}").map_err(io("send request"))?;
        s.shutdown(std::net::Shutdown::Write)
            .map_err(io("send request"))?;
        let mut buf = Vec::new();
        s.take(MAX_REPLY + 1)
            .read_to_end(&mut buf)
            .map_err(io("read reply"))?;
        parse_reply(&buf)
    }
}

/// Exactly `{"decision":"allow"|"deny"}` and a newline, nothing else.
fn parse_reply(buf: &[u8]) -> Result<bool> {
    let bad = Error::Refused("malformed reply from the compositor");
    if buf.len() as u64 > MAX_REPLY {
        return Err(bad);
    }
    let line = buf.strip_suffix(b"\n").ok_or(bad.clone())?;
    let serde_json::Value::Object(m) = serde_json::from_slice(line).map_err(|_| bad.clone())? else {
        return Err(bad);
    };
    match (m.len(), m.get("decision").and_then(|d| d.as_str())) {
        (1, Some("allow")) => Ok(true),
        (1, Some("deny")) => Ok(false),
        _ => Err(bad),
    }
}

/// Never allows.
pub struct DenyConfirm;

impl Confirm for DenyConfirm {
    fn confirm_erase(&self, _disk: &Disk) -> Result<bool> {
        Ok(false)
    }
}

/// Test-only. Not compiled into the binary.
#[cfg(test)]
pub struct AllowConfirm;

#[cfg(test)]
impl Confirm for AllowConfirm {
    fn confirm_erase(&self, _disk: &Disk) -> Result<bool> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::net::UnixListener;

    fn disk() -> Disk {
        Disk {
            by_id: "nvme-X_1".into(),
            model: "X".into(),
            size_bytes: 1000,
            partitions: vec![],
        }
    }

    /// A listener that answers with `reply`; returns what it was sent.
    fn serve(reply: &'static [u8]) -> (TempDir, SocketConfirm, std::thread::JoinHandle<String>) {
        let t = TempDir::new();
        let socket = t.path().join("trusted.sock");
        let l = UnixListener::bind(&socket).unwrap();
        let h = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = String::new();
            c.read_to_string(&mut got).unwrap();
            let _ = c.write_all(reply);
            got
        });
        let c = SocketConfirm {
            socket,
            proc: "/proc".into(),
            exe: std::env::current_exe().unwrap(),
            uid: rustix::process::getuid().as_raw(),
        };
        (t, c, h)
    }

    #[test]
    fn allow_and_deny_round_trip() {
        for (reply, want) in [
            (&b"{\"decision\":\"allow\"}\n"[..], true),
            (b"{\"decision\":\"deny\"}\n", false),
        ] {
            let (_t, c, h) = serve(reply);
            assert_eq!(c.confirm_erase(&disk()), Ok(want));
            let sent = h.join().unwrap();
            assert_eq!(
                sent,
                "{\"action\":\"erase-disk\",\"disk\":\"nvme-X_1\",\"model\":\"X\",\"size_bytes\":1000}\n"
            );
        }
    }

    #[test]
    fn a_peer_running_another_binary_is_refused_before_anything_is_sent() {
        let (_t, mut c, h) = serve(b"{\"decision\":\"allow\"}\n");
        c.exe = "/usr/bin/abyss-not-us".into();
        assert!(c.confirm_erase(&disk()).is_err());
        assert_eq!(
            h.join().unwrap(),
            "",
            "the request must not reach an unverified peer"
        );
    }

    #[test]
    fn a_peer_of_another_uid_is_refused() {
        let (_t, mut c, h) = serve(b"{\"decision\":\"allow\"}\n");
        c.uid += 1;
        assert!(c.confirm_erase(&disk()).is_err());
        let _ = h.join();
    }

    #[test]
    fn anything_but_one_exact_line_is_an_error() {
        for bad in [
            &b"{\"decision\":\"allow\"}"[..],
            b"{\"decision\":\"allow\",\"x\":1}\n",
            b"{\"decision\":\"ALLOW\"}\n",
            b"{\"decision\":\"allow\"}\n{\"decision\":\"allow\"}\n",
            b"allow\n",
            b"",
        ] {
            assert!(parse_reply(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn no_compositor_means_no_confirmation() {
        let c = SocketConfirm {
            socket: "/nonexistent/trusted.sock".into(),
            ..SocketConfirm::for_caller(1000)
        };
        assert!(c.confirm_erase(&disk()).is_err());
    }
}
