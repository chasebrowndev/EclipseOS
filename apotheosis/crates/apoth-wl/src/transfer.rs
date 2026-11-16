// SPDX-License-Identifier: AGPL-3.0-only
//! Non-blocking clipboard / primary-selection transfers (T-09 §6): pipe sources on the
//! loop with a size cap and a timeout. A [`Transfer`] is a calloop event source; insert it
//! with [`Transfer::insert`]. Content is never logged: only sizes and ids.

use std::fmt;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::{
    EventSource, InsertError, Interest, LoopHandle, Mode, Poll, PostAction, Readiness,
    RegistrationToken, Token, TokenFactory,
};
use rustix::io::Errno;

use crate::clock::{Stamp, Timerfd};
use crate::event::{SeatId, Secret, SelectionKind};
use crate::mime::Payload;

/// Size cap and timeout of one transfer. T-09 gives no numbers (`paste.max-bytes`, 5 MiB,
/// is a paste-guard limit above this crate), so the transfer cap is a conservative multiple.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferLimits {
    /// Most bytes accepted (receive) or sent.
    pub max_bytes: usize,
    /// Wall time allowed for the whole transfer.
    pub timeout: Duration,
}

impl Default for TransferLimits {
    fn default() -> Self {
        TransferLimits {
            max_bytes: 16 * 1024 * 1024,
            timeout: Duration::from_secs(5),
        }
    }
}

/// Direction of a transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferDirection {
    /// We read a selection another client owns.
    Receive,
    /// We write our selection to a requesting client.
    Send,
}

/// How a transfer ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferStatus {
    /// All data moved.
    Complete,
    /// The size cap was exceeded; the data is discarded.
    TooLarge,
    /// The timeout passed.
    TimedOut,
    /// An I/O error.
    Failed(io::ErrorKind),
}

/// Result delivered when a transfer ends.
#[derive(Debug)]
pub struct TransferDone {
    /// Transfer id (matches [`Transfer::id`]).
    pub id: u64,
    /// Seat.
    pub seat: SeatId,
    /// Clipboard or primary.
    pub kind: SelectionKind,
    /// Direction.
    pub direction: TransferDirection,
    /// What the data is (receive).
    pub payload: Payload,
    /// How it ended.
    pub status: TransferStatus,
    /// Bytes moved.
    pub bytes: usize,
    /// Received data (empty for sends and for failed transfers).
    pub data: Secret<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Pending,
    Done(TransferStatus),
}

const CHUNK: usize = 16 * 1024;
const MAX_CHUNKS_PER_STEP: usize = 64;

fn kind_of(e: Errno) -> io::ErrorKind {
    io::Error::from(e).kind()
}

/// The transfer state machine, separate from the loop plumbing so it can be tested on pipes.
pub(crate) struct Core {
    pub(crate) direction: TransferDirection,
    pub(crate) limits: TransferLimits,
    pub(crate) deadline: Stamp,
    pub(crate) received: Vec<u8>,
    pub(crate) outgoing: Arc<[u8]>,
    pub(crate) written: usize,
}

impl Core {
    pub(crate) fn receiving(limits: TransferLimits, now: Stamp) -> Core {
        Core {
            direction: TransferDirection::Receive,
            limits,
            deadline: now.plus_ns(limits.timeout.as_nanos() as u64),
            received: Vec::new(),
            outgoing: Arc::from(Vec::new()),
            written: 0,
        }
    }

    pub(crate) fn sending(data: Arc<[u8]>, limits: TransferLimits, now: Stamp) -> Core {
        Core {
            direction: TransferDirection::Send,
            limits,
            deadline: now.plus_ns(limits.timeout.as_nanos() as u64),
            received: Vec::new(),
            outgoing: data,
            written: 0,
        }
    }

    pub(crate) fn step_read(&mut self, fd: impl AsFd) -> Step {
        let fd = fd.as_fd();
        let mut chunk = [0u8; CHUNK];
        for _ in 0..MAX_CHUNKS_PER_STEP {
            match rustix::io::read(fd, &mut chunk[..]) {
                Ok(0) => return Step::Done(TransferStatus::Complete),
                Ok(n) => {
                    if self.received.len() + n > self.limits.max_bytes {
                        self.received = Vec::new();
                        return Step::Done(TransferStatus::TooLarge);
                    }
                    self.received.extend_from_slice(&chunk[..n]);
                }
                Err(Errno::AGAIN) => return Step::Pending,
                Err(Errno::INTR) => {}
                Err(e) => return Step::Done(TransferStatus::Failed(kind_of(e))),
            }
        }
        Step::Pending
    }

    pub(crate) fn step_write(&mut self, fd: impl AsFd) -> Step {
        let fd = fd.as_fd();
        if self.outgoing.len() > self.limits.max_bytes {
            return Step::Done(TransferStatus::TooLarge);
        }
        for _ in 0..MAX_CHUNKS_PER_STEP {
            if self.written >= self.outgoing.len() {
                return Step::Done(TransferStatus::Complete);
            }
            let end = (self.written + CHUNK).min(self.outgoing.len());
            match rustix::io::write(fd, &self.outgoing[self.written..end]) {
                Ok(n) => self.written += n,
                Err(Errno::AGAIN) => return Step::Pending,
                Err(Errno::INTR) => {}
                Err(e) => return Step::Done(TransferStatus::Failed(kind_of(e))),
            }
        }
        Step::Pending
    }

    pub(crate) fn step(&mut self, fd: impl AsFd, now: Stamp) -> Step {
        let s = match self.direction {
            TransferDirection::Receive => self.step_read(fd),
            TransferDirection::Send => self.step_write(fd),
        };
        match s {
            Step::Pending if now >= self.deadline => Step::Done(TransferStatus::TimedOut),
            other => other,
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        match self.direction {
            TransferDirection::Receive => self.received.len(),
            TransferDirection::Send => self.written,
        }
    }
}

/// A transfer ready to be inserted in the loop.
pub struct Transfer {
    id: u64,
    seat: SeatId,
    kind: SelectionKind,
    payload: Payload,
    core: Core,
    pipe: Generic<OwnedFd>,
    timer: Generic<Timerfd>,
    finished: bool,
}

impl fmt::Debug for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transfer")
            .field("id", &self.id)
            .field("seat", &self.seat)
            .field("kind", &self.kind)
            .field("direction", &self.core.direction)
            .finish_non_exhaustive()
    }
}

fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(fd)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

impl Transfer {
    fn build(
        id: u64,
        seat: SeatId,
        kind: SelectionKind,
        payload: Payload,
        core: Core,
        fd: OwnedFd,
        interest: Interest,
    ) -> io::Result<Transfer> {
        set_nonblocking(&fd)?;
        let timer = Timerfd::new()?;
        timer.arm_at(core.deadline)?;
        Ok(Transfer {
            id,
            seat,
            kind,
            payload,
            core,
            pipe: Generic::new(fd, interest, Mode::Level),
            timer: Generic::new(timer, Interest::READ, Mode::Level),
            finished: false,
        })
    }

    /// Read end of a pipe whose write end went to the selection owner.
    pub(crate) fn receive(
        id: u64,
        seat: SeatId,
        kind: SelectionKind,
        payload: Payload,
        read_fd: OwnedFd,
        limits: TransferLimits,
    ) -> io::Result<Transfer> {
        let core = Core::receiving(limits, Stamp::now());
        Transfer::build(id, seat, kind, payload, core, read_fd, Interest::READ)
    }

    /// Write end handed to us by a `send` request.
    pub(crate) fn send(
        id: u64,
        seat: SeatId,
        kind: SelectionKind,
        data: Arc<[u8]>,
        write_fd: OwnedFd,
        limits: TransferLimits,
    ) -> io::Result<Transfer> {
        let core = Core::sending(data, limits, Stamp::now());
        Transfer::build(
            id,
            seat,
            kind,
            Payload::Text,
            core,
            write_fd,
            Interest::WRITE,
        )
    }

    /// Transfer id.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Direction.
    pub fn direction(&self) -> TransferDirection {
        self.core.direction
    }

    /// Insert into the loop; `on_done` runs once when the transfer ends.
    #[allow(clippy::result_large_err)] // calloop's InsertError carries the source back by value
    pub fn insert<'l, D>(
        self,
        handle: &LoopHandle<'l, D>,
        mut on_done: impl FnMut(TransferDone, &mut D) + 'l,
    ) -> Result<RegistrationToken, InsertError<Transfer>> {
        handle.insert_source(self, move |done, _, data| on_done(done, data))
    }

    fn done(&mut self, status: TransferStatus) -> TransferDone {
        self.finished = true;
        let data = if status == TransferStatus::Complete
            && self.core.direction == TransferDirection::Receive
        {
            std::mem::take(&mut self.core.received)
        } else {
            Vec::new()
        };
        TransferDone {
            id: self.id,
            seat: self.seat,
            kind: self.kind,
            direction: self.core.direction,
            payload: self.payload,
            status,
            bytes: self.core.bytes(),
            data: Secret(data),
        }
    }
}

impl EventSource for Transfer {
    type Event = TransferDone;
    type Metadata = ();
    type Ret = ();
    type Error = calloop::Error;

    fn process_events<F>(
        &mut self,
        _readiness: Readiness,
        _token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        if self.finished {
            return Ok(PostAction::Remove);
        }
        self.timer.get_ref().drain();
        // Readiness is not distinguished by token: the step is non-blocking, so trying is cheap.
        // The pipe fd is borrowed out of the Generic for the step.
        let step = {
            let fd = self.pipe.get_ref().as_fd();
            self.core.step(fd, Stamp::now())
        };
        match step {
            Step::Pending => Ok(PostAction::Continue),
            Step::Done(status) => {
                let done = self.done(status);
                tracing::debug!(
                    id = done.id,
                    bytes = done.bytes,
                    status = ?done.status,
                    "selection transfer ended"
                );
                callback(done, &mut ());
                Ok(PostAction::Remove)
            }
        }
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.pipe.register(poll, token_factory)?;
        self.timer.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.pipe.reregister(poll, token_factory)?;
        self.timer.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.pipe.unregister(poll)?;
        self.timer.unregister(poll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::pipe::{PipeFlags, pipe_with};

    fn pipe() -> (OwnedFd, OwnedFd) {
        let (r, w) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        set_nonblocking(&r).unwrap();
        set_nonblocking(&w).unwrap();
        (r, w)
    }

    fn limits(max: usize) -> TransferLimits {
        TransferLimits {
            max_bytes: max,
            timeout: Duration::from_millis(50),
        }
    }

    #[test]
    fn receive_reads_to_eof() {
        let (r, w) = pipe();
        rustix::io::write(&w, b"hello world").unwrap();
        drop(w);
        let mut c = Core::receiving(limits(1024), Stamp::now());
        assert_eq!(
            c.step(&r, Stamp::now()),
            Step::Done(TransferStatus::Complete)
        );
        assert_eq!(c.received, b"hello world");
    }

    #[test]
    fn receive_pending_while_writer_is_open() {
        let (r, w) = pipe();
        rustix::io::write(&w, b"abc").unwrap();
        let mut c = Core::receiving(limits(1024), Stamp::now());
        assert_eq!(c.step(&r, Stamp::now()), Step::Pending);
        assert_eq!(c.received, b"abc");
        drop(w);
        assert_eq!(
            c.step(&r, Stamp::now()),
            Step::Done(TransferStatus::Complete)
        );
    }

    #[test]
    fn receive_enforces_the_size_cap() {
        let (r, w) = pipe();
        rustix::io::write(&w, &[b'x'; 100]).unwrap();
        drop(w);
        let mut c = Core::receiving(limits(50), Stamp::now());
        assert_eq!(
            c.step(&r, Stamp::now()),
            Step::Done(TransferStatus::TooLarge)
        );
        assert!(c.received.is_empty());
    }

    #[test]
    fn receive_times_out() {
        let (r, _w) = pipe();
        let t0 = Stamp::now();
        let mut c = Core::receiving(limits(1024), t0);
        assert_eq!(c.step(&r, t0), Step::Pending);
        assert_eq!(
            c.step(&r, t0.plus_ms(60)),
            Step::Done(TransferStatus::TimedOut)
        );
    }

    #[test]
    fn send_handles_a_full_pipe_in_steps() {
        let (r, w) = pipe();
        let data: Arc<[u8]> = Arc::from(vec![7u8; 300 * 1024]);
        let mut c = Core::sending(data.clone(), limits(1 << 20), Stamp::now());
        let mut read_back = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut guard = 0;
        loop {
            match c.step(&w, Stamp::now()) {
                Step::Done(s) => {
                    assert_eq!(s, TransferStatus::Complete);
                    break;
                }
                Step::Pending => {
                    // drain the reader so the writer can progress
                    while let Ok(n) = rustix::io::read(&r, &mut buf[..]) {
                        if n == 0 {
                            break;
                        }
                        read_back.extend_from_slice(&buf[..n]);
                    }
                }
            }
            guard += 1;
            assert!(guard < 1000, "send never finished");
        }
        drop(w);
        while let Ok(n) = rustix::io::read(&r, &mut buf[..]) {
            if n == 0 {
                break;
            }
            read_back.extend_from_slice(&buf[..n]);
        }
        assert_eq!(read_back.len(), data.len());
        assert_eq!(c.bytes(), data.len());
    }

    #[test]
    fn send_rejects_over_cap() {
        let (_r, w) = pipe();
        let data: Arc<[u8]> = Arc::from(vec![1u8; 100]);
        let mut c = Core::sending(data, limits(10), Stamp::now());
        assert_eq!(
            c.step(&w, Stamp::now()),
            Step::Done(TransferStatus::TooLarge)
        );
    }

    #[test]
    fn send_to_a_closed_reader_fails_with_broken_pipe() {
        let (r, w) = pipe();
        drop(r);
        let data: Arc<[u8]> = Arc::from(vec![1u8; 100]);
        let mut c = Core::sending(data, limits(1000), Stamp::now());
        assert_eq!(
            c.step(&w, Stamp::now()),
            Step::Done(TransferStatus::Failed(io::ErrorKind::BrokenPipe))
        );
    }

    #[test]
    fn transfer_debug_never_prints_content() {
        let (r, w) = pipe();
        drop(w);
        let t = Transfer::receive(
            9,
            SeatId(1),
            SelectionKind::Clipboard,
            Payload::Text,
            r,
            limits(10),
        )
        .unwrap();
        let s = format!("{t:?}");
        assert!(s.contains("id: 9"));
        let d = TransferDone {
            id: 1,
            seat: SeatId(1),
            kind: SelectionKind::Primary,
            direction: TransferDirection::Receive,
            payload: Payload::Text,
            status: TransferStatus::Complete,
            bytes: 6,
            data: Secret(b"secret".to_vec()),
        };
        assert!(!format!("{d:?}").contains("secret"));
    }
}
