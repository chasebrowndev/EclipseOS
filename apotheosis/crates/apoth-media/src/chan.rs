// SPDX-License-Identifier: AGPL-3.0-only
//! One end of the terminal ↔ helper socket: framed messages ([`crate::wire`]) plus the fds
//! that ride with them (`SCM_RIGHTS`, received into a FIFO the message handler pops).

use std::collections::VecDeque;
use std::io::{self, IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};

use crate::wire::{self, WireError};

/// Fds received but not yet claimed, at most.
const MAX_QUEUED_FDS: usize = 4;
/// Read size per `recvmsg`.
const READ: usize = 64 << 10;

#[derive(Debug)]
pub struct Chan {
    s: UnixStream,
    buf: Vec<u8>,
    start: usize,
    fds: VecDeque<OwnedFd>,
    /// Largest frame this end accepts (the terminal is stricter than [`wire::MAX_MESSAGE`]).
    max: usize,
}

impl Chan {
    pub fn new(s: UnixStream, max: usize) -> Chan {
        Chan {
            s,
            buf: Vec::new(),
            start: 0,
            fds: VecDeque::new(),
            max: max.min(wire::MAX_MESSAGE),
        }
    }

    pub fn stream(&self) -> &UnixStream {
        &self.s
    }

    /// Send one encoded message, with `fd` attached to its first byte.
    pub fn send(&self, msg: &[u8], fd: Option<BorrowedFd<'_>>) -> io::Result<()> {
        let mut rest = msg;
        if let Some(fd) = fd {
            let fds = [fd];
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut ctl = SendAncillaryBuffer::new(&mut space);
            if !ctl.push(SendAncillaryMessage::ScmRights(&fds)) {
                return Err(io::Error::other("cmsg space"));
            }
            let n = loop {
                match sendmsg(
                    &self.s,
                    &[IoSlice::new(rest)],
                    &mut ctl,
                    SendFlags::NOSIGNAL,
                ) {
                    Ok(n) => break n,
                    Err(rustix::io::Errno::INTR) => {}
                    Err(e) => return Err(e.into()),
                }
            };
            rest = &rest[n..];
        }
        (&self.s).write_all(rest)
    }

    /// The next whole frame already buffered, if any.
    fn frame(&mut self) -> io::Result<Option<(u8, Vec<u8>)>> {
        let avail = &self.buf[self.start..];
        if avail.len() >= 4 {
            let n = u32::from_le_bytes([avail[0], avail[1], avail[2], avail[3]]) as usize;
            if n > self.max {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "frame too large",
                ));
            }
        }
        match wire::split(avail) {
            Ok((tag, body, used)) => {
                let out = (tag, body.to_vec());
                self.start += used;
                if self.start == self.buf.len() {
                    self.buf.clear();
                    self.start = 0;
                }
                Ok(Some(out))
            }
            Err(WireError::Short) => Ok(None),
            Err(WireError::Bad) => Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame")),
        }
    }

    /// A whole message is buffered (no read needed).
    pub fn buffered(&self) -> bool {
        wire::split(&self.buf[self.start..]).is_ok()
    }

    /// Block until one message arrives. `Ok(None)`: the peer closed.
    pub fn recv(&mut self) -> io::Result<Option<(u8, Vec<u8>)>> {
        loop {
            if let Some(f) = self.frame()? {
                return Ok(Some(f));
            }
            if self.start > 0 {
                self.buf.drain(..self.start);
                self.start = 0;
            }
            let old = self.buf.len();
            self.buf.resize(old + READ, 0);
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
            let mut ctl = RecvAncillaryBuffer::new(&mut space);
            let r = recvmsg(
                &self.s,
                &mut [IoSliceMut::new(&mut self.buf[old..])],
                &mut ctl,
                RecvFlags::CMSG_CLOEXEC,
            );
            let n = match r {
                Ok(m) => m.bytes,
                Err(rustix::io::Errno::INTR) => {
                    self.buf.truncate(old);
                    continue;
                }
                Err(e) => {
                    self.buf.truncate(old);
                    return Err(e.into());
                }
            };
            self.buf.truncate(old + n);
            for m in ctl.drain() {
                if let RecvAncillaryMessage::ScmRights(fds) = m {
                    for fd in fds {
                        if self.fds.len() >= MAX_QUEUED_FDS {
                            return Err(io::Error::new(io::ErrorKind::InvalidData, "fds"));
                        }
                        self.fds.push_back(fd);
                    }
                }
            }
            if n == 0 {
                return if self.buf.len() == self.start {
                    Ok(None)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
        }
    }

    /// The oldest received fd not yet claimed.
    pub fn take_fd(&mut self) -> Option<OwnedFd> {
        self.fds.pop_front()
    }

    /// Wait up to `ms` for input (true: a message is buffered or the socket is readable).
    pub fn readable(&self, ms: i32) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        if self.buffered() {
            return true;
        }
        let ts = (ms >= 0).then(|| Timespec {
            tv_sec: i64::from(ms / 1000),
            tv_nsec: i64::from(ms % 1000) * 1_000_000,
        });
        let mut fds = [PollFd::new(&self.s, PollFlags::IN)];
        loop {
            match poll(&mut fds, ts.as_ref()) {
                Ok(n) => return n > 0,
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return true,
            }
        }
    }
}

impl AsFd for Chan {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.s.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::ToHelper;

    #[test]
    fn messages_and_fds_cross_in_order() {
        let (a, b) = UnixStream::pair().unwrap();
        let a = Chan::new(a, 1 << 20);
        let mut b = Chan::new(b, 1 << 20);
        let memfd = rustix::fs::memfd_create("t", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        a.send(
            &ToHelper::Ring {
                slots: 2,
                slot_bytes: 8,
            }
            .encode(),
            Some(memfd.as_fd()),
        )
        .unwrap();
        a.send(&ToHelper::Release { slot: 1 }.encode(), None)
            .unwrap();
        let (t, body) = b.recv().unwrap().unwrap();
        assert_eq!(
            ToHelper::decode(t, &body).unwrap(),
            ToHelper::Ring {
                slots: 2,
                slot_bytes: 8
            }
        );
        assert!(b.take_fd().is_some());
        let (t, body) = b.recv().unwrap().unwrap();
        assert_eq!(
            ToHelper::decode(t, &body).unwrap(),
            ToHelper::Release { slot: 1 }
        );
        assert!(b.take_fd().is_none());
        drop(a);
        assert!(b.recv().unwrap().is_none());
    }

    #[test]
    fn an_oversized_frame_is_refused_before_it_is_read() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut b = Chan::new(b, 16);
        (&a).write_all(&100u32.to_le_bytes()).unwrap();
        assert!(b.recv().is_err());
    }
}
