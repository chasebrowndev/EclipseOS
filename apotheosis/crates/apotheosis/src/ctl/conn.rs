// SPDX-License-Identifier: AGPL-3.0-only
//! One client connection's buffers (T-12 §1): NDJSON in through the bounded line decoder,
//! whole lines out through a bounded buffer (a reply or event is queued whole, so events
//! never interleave inside a reply). A reader that falls past the high-water mark is
//! disconnected rather than buffered without bound.

use std::collections::VecDeque;
use std::io::{self, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags};

use apoth_action::EventKind;
use apoth_proto::{LineDecoder, WireError};

/// Output a client may leave unread before it is disconnected.
pub(crate) const HIGH_WATER: usize = 4 * 1024 * 1024;
/// Bytes read per wakeup (the source is level-triggered; the rest waits a turn).
const READ_BUDGET: usize = 256 * 1024;
/// Subscriptions per connection.
pub(crate) const MAX_SUBS: usize = 64;
/// Descriptors (`SCM_RIGHTS`) a connection may hold unclaimed; extras are closed.
pub(crate) const MAX_FDS: usize = 4;

/// A subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sub {
    /// Its id on this connection.
    pub(crate) id: u64,
    /// Event kinds delivered.
    pub(crate) kinds: Vec<EventKind>,
    /// Only events of this pane (from the `match`), or all.
    pub(crate) pane: Option<u64>,
}

/// What a read produced.
#[derive(Debug, Default)]
pub(crate) struct Input {
    /// Complete lines (or a framing error for an oversize line).
    pub(crate) lines: Vec<Result<Vec<u8>, WireError>>,
    /// The peer closed its end.
    pub(crate) eof: bool,
}

/// Buffers and protocol state of one connection.
#[derive(Debug)]
pub(crate) struct Conn {
    stream: UnixStream,
    dec: LineDecoder,
    out: Vec<u8>,
    sent: usize,
    /// Negotiated protocol version after `hello`.
    pub(crate) proto: Option<u32>,
    /// Live subscriptions.
    pub(crate) subs: Vec<Sub>,
    next_sub: u64,
    /// Event sequence number on this connection.
    pub(crate) seq: u64,
    /// Descriptors received with the stream (`media.attach` claims the oldest).
    fds: VecDeque<OwnedFd>,
}

impl Conn {
    /// Wrap an accepted, non-blocking stream.
    #[must_use]
    pub(crate) fn new(stream: UnixStream) -> Conn {
        Conn {
            stream,
            dec: LineDecoder::new(),
            out: Vec::new(),
            sent: 0,
            proto: None,
            subs: Vec::new(),
            next_sub: 1,
            seq: 0,
            fds: VecDeque::new(),
        }
    }

    /// The socket.
    #[must_use]
    pub(crate) fn stream(&self) -> &UnixStream {
        &self.stream
    }

    /// Read what is available (up to a budget) and split it into lines.
    pub(crate) fn read(&mut self) -> io::Result<Input> {
        let mut input = Input::default();
        let mut buf = [0u8; 16 * 1024];
        let mut total = 0;
        while total < READ_BUDGET {
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
            let mut cmsg = RecvAncillaryBuffer::new(&mut space);
            let got = rustix::net::recvmsg(
                &self.stream,
                &mut [IoSliceMut::new(&mut buf)],
                &mut cmsg,
                RecvFlags::CMSG_CLOEXEC | RecvFlags::DONTWAIT,
            )
            .map(|m| m.bytes)
            .map_err(io::Error::from);
            for msg in cmsg.drain() {
                if let RecvAncillaryMessage::ScmRights(fds) = msg {
                    for fd in fds {
                        // Past the cap the descriptor drops (closes) here.
                        if self.fds.len() < MAX_FDS {
                            self.fds.push_back(fd);
                        }
                    }
                }
            }
            match got {
                Ok(0) => {
                    input.eof = true;
                    break;
                }
                Ok(n) => {
                    total += n;
                    input.lines.extend(self.dec.feed(&buf[..n]));
                    if self.dec.is_poisoned() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(input)
    }

    /// The oldest descriptor the client sent and nothing has claimed.
    pub(crate) fn take_fd(&mut self) -> Option<OwnedFd> {
        self.fds.pop_front()
    }

    /// The decoder hit an oversize line: nothing more is read from this client.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn poisoned(&self) -> bool {
        self.dec.is_poisoned()
    }

    /// Queue one whole line. `false` when the client has fallen past the high-water mark
    /// (the caller disconnects it).
    pub(crate) fn queue(&mut self, line: &[u8]) -> bool {
        if self.pending() + line.len() > HIGH_WATER {
            return false;
        }
        if self.sent > 0 && self.sent == self.out.len() {
            self.out.clear();
            self.sent = 0;
        }
        self.out.extend_from_slice(line);
        true
    }

    /// Bytes queued and not yet written.
    #[must_use]
    pub(crate) fn pending(&self) -> usize {
        self.out.len() - self.sent
    }

    /// Write as much as the socket takes. `Ok(true)` once everything queued is written.
    pub(crate) fn flush(&mut self) -> io::Result<bool> {
        while self.sent < self.out.len() {
            match self.stream.write(&self.out[self.sent..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => self.sent += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.out.clear();
        self.sent = 0;
        Ok(true)
    }

    /// Add a subscription; `None` past [`MAX_SUBS`].
    pub(crate) fn subscribe(&mut self, kinds: Vec<EventKind>, pane: Option<u64>) -> Option<u64> {
        if self.subs.len() >= MAX_SUBS {
            return None;
        }
        let id = self.next_sub;
        self.next_sub += 1;
        self.subs.push(Sub { id, kinds, pane });
        Some(id)
    }

    /// Drop a subscription; `false` if it did not exist.
    pub(crate) fn unsubscribe(&mut self, id: u64) -> bool {
        let n = self.subs.len();
        self.subs.retain(|s| s.id != id);
        self.subs.len() != n
    }

    /// The subscription that takes an event of `kind` about `pane`, if any.
    #[must_use]
    pub(crate) fn wants(&self, kind: EventKind, pane: Option<u64>) -> Option<u64> {
        self.subs
            .iter()
            .find(|s| s.kinds.contains(&kind) && (s.pane.is_none() || s.pane == pane))
            .map(|s| s.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_proto::{ErrorCode, MAX_LINE_BYTES};
    use std::io::Read;

    fn pair() -> (Conn, UnixStream) {
        let (a, b) = UnixStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        (Conn::new(a), b)
    }

    #[test]
    fn descriptors_queue_with_the_line_and_extras_close() {
        use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags};
        use std::os::fd::AsFd;
        let (mut c, peer) = pair();
        let pipes: Vec<_> = (0..MAX_FDS + 1)
            .map(|_| rustix::pipe::pipe().unwrap().0)
            .collect();
        let fds: Vec<_> = pipes.iter().map(AsFd::as_fd).collect();
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS + 1))];
        let mut cmsg = SendAncillaryBuffer::new(&mut space);
        assert!(cmsg.push(SendAncillaryMessage::ScmRights(&fds)));
        let line = b"{\"id\":1,\"method\":\"media.attach\"}\n";
        rustix::net::sendmsg(
            &peer,
            &[io::IoSlice::new(line)],
            &mut cmsg,
            SendFlags::empty(),
        )
        .unwrap();
        let i = c.read().unwrap();
        assert_eq!(i.lines.len(), 1);
        for _ in 0..MAX_FDS {
            assert!(c.take_fd().is_some());
        }
        assert!(c.take_fd().is_none());
    }

    #[test]
    fn lines_split_across_reads() {
        let (mut c, mut peer) = pair();
        peer.write_all(b"{\"id\":1,").unwrap();
        let i = c.read().unwrap();
        assert!(i.lines.is_empty() && !i.eof);
        peer.write_all(b"\"method\":\"version\"}\n{\"id\":2}\n")
            .unwrap();
        let i = c.read().unwrap();
        assert_eq!(i.lines.len(), 2);
        drop(peer);
        assert!(c.read().unwrap().eof);
    }

    #[test]
    fn oversize_line_is_an_error_and_poisons() {
        let (mut c, peer) = pair();
        let big = vec![b'x'; MAX_LINE_BYTES + 10];
        let w = std::thread::spawn(move || {
            let mut peer = peer;
            let _ = peer.write_all(&big);
            peer
        });
        let mut errs = Vec::new();
        while errs.is_empty() {
            let i = c.read().unwrap();
            errs.extend(i.lines.into_iter().filter_map(Result::err));
        }
        assert_eq!(errs[0].code, ErrorCode::LineTooLong);
        assert!(c.poisoned());
        drop(c);
        let _ = w.join();
    }

    #[test]
    fn slow_reader_hits_the_high_water_mark() {
        let (mut c, _peer) = pair();
        let line = vec![b'y'; 64 * 1024];
        let mut queued = 0usize;
        loop {
            if !c.queue(&line) {
                break;
            }
            let _ = c.flush().unwrap();
            queued += 1;
            assert!(queued < 10_000, "never reached the high-water mark");
        }
        assert!(c.pending() + line.len() > HIGH_WATER);
    }

    #[test]
    fn whole_lines_in_order() {
        let (mut c, mut peer) = pair();
        assert!(c.queue(b"{\"id\":1,\"ok\":null}\n"));
        assert!(c.queue(b"{\"event\":\"bell\",\"seq\":1}\n"));
        assert!(c.flush().unwrap());
        let mut got = vec![0u8; 64];
        let n = peer.read(&mut got).unwrap();
        assert_eq!(
            &got[..n],
            b"{\"id\":1,\"ok\":null}\n{\"event\":\"bell\",\"seq\":1}\n"
        );
    }

    #[test]
    fn subscriptions() {
        let (mut c, _p) = pair();
        let a = c.subscribe(vec![EventKind::Bell], Some(3)).unwrap();
        let b = c.subscribe(vec![EventKind::PaneOpened], None).unwrap();
        assert_eq!(c.wants(EventKind::Bell, Some(3)), Some(a));
        assert_eq!(c.wants(EventKind::Bell, Some(4)), None);
        assert_eq!(c.wants(EventKind::PaneOpened, Some(9)), Some(b));
        assert!(c.unsubscribe(a));
        assert!(!c.unsubscribe(a));
        assert_eq!(c.wants(EventKind::Bell, Some(3)), None);
    }
}
