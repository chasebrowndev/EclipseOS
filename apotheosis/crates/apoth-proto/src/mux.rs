// SPDX-License-Identifier: Apache-2.0
//! The mux attach protocol (T-21 §3–§4, ADR 0123): what a view and `apotheosis --server`
//! exchange on `server-<session>.sock`.
//!
//! Binary and record-framed: `len: u32 LE | kind: u8 | payload`, `len` counting the kind
//! byte and the payload, at most [`MAX_RECORD`]. Every stream opens with [`ViewMsg::Hello`]
//! (major, minor, capabilities); a server of another major version answers
//! [`ServerMsg::Refused`] with a clear reason and closes (§4). Within a major version an
//! unknown record kind is skipped whole ([`Decoded::Skipped`]), so later minors can add
//! records. Every field is length-checked; nothing here allocates more than the record it
//! has already received, and a record is never longer than [`MAX_RECORD`].
//!
//! Sync: after `Attach` the server sends, per pane it grants, `SnapBegin` (the terminal's
//! state record and the tee sequence number the next stream record has), the screens'
//! pages as `SnapPage`s and `SnapEnd`; then the pane's output as `Tee` records, with
//! `Resized` in the same sequence space so a view applies a size change at exactly the
//! point of the byte stream the server did. A view that sees a gap asks `Resync`.
//!
//! No content is logged by this module or by anything that uses it for diagnostics: record
//! kinds, pane ids, sizes and counts only.

/// Major protocol version: a different major is refused (T-21 §4).
pub const MUX_MAJOR: u16 = 1;
/// Minor protocol version: informational; unknown records of a newer minor are skipped.
pub const MUX_MINOR: u16 = 1;
/// Longest record (kind byte and payload): one page of a full-size alternate screen.
pub const MAX_RECORD: usize = 64 * 1024 * 1024 + 4096;
/// Longest `Tee` / `Input` payload a sender puts in one record.
pub const MAX_CHUNK: usize = 64 * 1024;
/// Most capability strings in a hello.
pub const MAX_MUX_CAPS: usize = 32;
/// Longest short string (capability, client name, session name, reason).
pub const MAX_MUX_STR: usize = 1024;
/// Most panes in one `Panes` record.
pub const MAX_PANES: usize = 4096;
/// Most argv elements in a `Spawn`.
pub const MAX_SPAWN_ARGV: usize = 4096;
/// Longest argv element or cwd in a `Spawn`.
pub const MAX_SPAWN_ELEM: usize = 64 * 1024;

/// A view's message to the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewMsg {
    /// First record of every stream.
    Hello {
        /// [`MUX_MAJOR`] of the sender.
        major: u16,
        /// [`MUX_MINOR`] of the sender.
        minor: u16,
        /// Capability names (none are defined in 1.0).
        caps: Vec<String>,
        /// The client's name (`apotheosis`, `apoth`); informational, never trusted.
        client: String,
    },
    /// Become a view: snapshots and the tee of every pane granted.
    Attach,
    /// The pane list only (`apoth server ls`): no content, no tee.
    List,
    /// Bytes for a pane's PTY (only for panes granted `input`). Never logged.
    Input {
        /// Pane id.
        pane: u64,
        /// The bytes.
        bytes: Vec<u8>,
    },
    /// This view's size for a pane; the server decides by its resize policy.
    Resize {
        /// Pane id.
        pane: u64,
        /// Rows.
        rows: u16,
        /// Columns.
        cols: u16,
        /// Cell width in pixels (0 = unknown).
        cell_w: u16,
        /// Cell height in pixels (0 = unknown).
        cell_h: u16,
    },
    /// Start a pane on the server. `argv` empty = the configured shell.
    Spawn {
        /// Opaque to the server's answer: echoed in [`ServerMsg::Spawned`].
        token: u64,
        /// Command; never logged.
        argv: Vec<String>,
        /// Working directory; never logged.
        cwd: Option<String>,
        /// Rows.
        rows: u16,
        /// Columns.
        cols: u16,
    },
    /// Hang up a pane (SIGHUP → SIGTERM → SIGKILL, T-18 §7).
    Close {
        /// Pane id.
        pane: u64,
    },
    /// Send a fresh snapshot of a pane (the view lost sync).
    Resync {
        /// Pane id.
        pane: u64,
    },
    /// Leave; the panes keep running.
    Detach,
    /// Detach every attached view (`apoth server detach`).
    DetachAll,
    /// Close every pane (HUP, TERM, KILL) and end the server: tmux `kill-session`
    /// (`apoth server kill`, the `session.kill` action). Minor 1.
    KillSession,
}

/// One pane in a [`ServerMsg::Panes`] list. Ids, sizes and state only: no titles, no
/// command lines, no working directories (those are content, T-12 §4 `observe`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneEntry {
    /// Pane id.
    pub pane: u64,
    /// Rows.
    pub rows: u16,
    /// Columns.
    pub cols: u16,
    /// False once the process exited.
    pub alive: bool,
    /// Views attached to it.
    pub views: u32,
}

/// The server's message to a view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerMsg {
    /// Answer to `Hello`.
    Welcome {
        /// Server [`MUX_MAJOR`].
        major: u16,
        /// Server [`MUX_MINOR`].
        minor: u16,
        /// Capabilities in force.
        caps: Vec<String>,
        /// The session name.
        session: String,
        /// This connection's view id.
        view: u64,
    },
    /// The stream is refused or ended; the server closes after it.
    Refused {
        /// Why, for the user (no content).
        reason: String,
    },
    /// The panes (after `List`, and first after `Attach`).
    Panes(Vec<PaneEntry>),
    /// A pane snapshot begins.
    SnapBegin {
        /// Pane id.
        pane: u64,
        /// Sequence number of the next `Tee`/`Resized` record of this pane.
        seq: u64,
        /// The view may not send input to this pane (granted `read-content` only).
        read_only: bool,
        /// The terminal state record (`apoth_term::TermImage::state`).
        state: Vec<u8>,
    },
    /// One page of a snapshot.
    SnapPage {
        /// Pane id.
        pane: u64,
        /// 0 primary, 1 alternate.
        screen: u8,
        /// The page bytes.
        bytes: Vec<u8>,
    },
    /// The snapshot of `pane` is complete.
    SnapEnd {
        /// Pane id.
        pane: u64,
    },
    /// PTY output, in order. Never logged.
    Tee {
        /// Pane id.
        pane: u64,
        /// Stream sequence number.
        seq: u64,
        /// The bytes.
        bytes: Vec<u8>,
    },
    /// The pane's terminal was resized here in its stream.
    Resized {
        /// Pane id.
        pane: u64,
        /// Stream sequence number.
        seq: u64,
        /// Rows.
        rows: u16,
        /// Columns.
        cols: u16,
    },
    /// A `Spawn` finished: the new pane's id (its snapshot follows), or `None` if refused.
    Spawned {
        /// The request's token.
        token: u64,
        /// The pane, or `None`.
        pane: Option<u64>,
    },
    /// The pane's process exited (status when known); the pane is gone from the session.
    Exited {
        /// Pane id.
        pane: u64,
        /// Exit status.
        status: Option<i32>,
    },
}

/// A decoding failure. The stream is unusable afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxError {
    /// A record's length is zero or past [`MAX_RECORD`].
    Length,
    /// A record's payload is malformed (truncated, trailing bytes, a bad bound).
    Malformed,
}

impl std::fmt::Display for MuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MuxError::Length => "mux record length out of range",
            MuxError::Malformed => "malformed mux record",
        })
    }
}

impl std::error::Error for MuxError {}

/// One decoded record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded<T> {
    /// A known record.
    Msg(T),
    /// A record of a kind this version does not know: skipped (T-21 §4).
    Skipped(u8),
}

// ---- kinds -------------------------------------------------------------------------------

mod kind {
    pub(super) const HELLO: u8 = 1;
    pub(super) const ATTACH: u8 = 2;
    pub(super) const LIST: u8 = 3;
    pub(super) const INPUT: u8 = 4;
    pub(super) const RESIZE: u8 = 5;
    pub(super) const SPAWN: u8 = 6;
    pub(super) const CLOSE: u8 = 7;
    pub(super) const RESYNC: u8 = 8;
    pub(super) const DETACH: u8 = 9;
    pub(super) const DETACH_ALL: u8 = 10;
    pub(super) const KILL_SESSION: u8 = 11;

    pub(super) const WELCOME: u8 = 64;
    pub(super) const REFUSED: u8 = 65;
    pub(super) const PANES: u8 = 66;
    pub(super) const SNAP_BEGIN: u8 = 67;
    pub(super) const SNAP_PAGE: u8 = 68;
    pub(super) const SNAP_END: u8 = 69;
    pub(super) const TEE: u8 = 70;
    pub(super) const RESIZED: u8 = 71;
    pub(super) const SPAWNED: u8 = 72;
    pub(super) const EXITED: u8 = 73;
}

// ---- encoding ----------------------------------------------------------------------------

struct Enc(Vec<u8>);

impl Enc {
    fn new(k: u8) -> Enc {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&[0, 0, 0, 0, k]);
        Enc(v)
    }
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(u8::from(v))
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
        self
    }
    fn str(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }
    fn strs(&mut self, v: &[String]) -> &mut Self {
        self.u32(v.len() as u32);
        for s in v {
            self.str(s);
        }
        self
    }
    fn done(mut self) -> Vec<u8> {
        let len = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&len.to_le_bytes());
        self.0
    }
}

impl ViewMsg {
    /// The framed record.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e;
        match self {
            ViewMsg::Hello {
                major,
                minor,
                caps,
                client,
            } => {
                e = Enc::new(kind::HELLO);
                e.u16(*major).u16(*minor).strs(caps).str(client);
            }
            ViewMsg::Attach => e = Enc::new(kind::ATTACH),
            ViewMsg::List => e = Enc::new(kind::LIST),
            ViewMsg::Input { pane, bytes } => {
                e = Enc::new(kind::INPUT);
                e.u64(*pane).bytes(bytes);
            }
            ViewMsg::Resize {
                pane,
                rows,
                cols,
                cell_w,
                cell_h,
            } => {
                e = Enc::new(kind::RESIZE);
                e.u64(*pane).u16(*rows).u16(*cols).u16(*cell_w).u16(*cell_h);
            }
            ViewMsg::Spawn {
                token,
                argv,
                cwd,
                rows,
                cols,
            } => {
                e = Enc::new(kind::SPAWN);
                e.u64(*token).strs(argv).bool(cwd.is_some());
                if let Some(c) = cwd {
                    e.str(c);
                }
                e.u16(*rows).u16(*cols);
            }
            ViewMsg::Close { pane } => {
                e = Enc::new(kind::CLOSE);
                e.u64(*pane);
            }
            ViewMsg::Resync { pane } => {
                e = Enc::new(kind::RESYNC);
                e.u64(*pane);
            }
            ViewMsg::Detach => e = Enc::new(kind::DETACH),
            ViewMsg::DetachAll => e = Enc::new(kind::DETACH_ALL),
            ViewMsg::KillSession => e = Enc::new(kind::KILL_SESSION),
        }
        e.done()
    }

    /// Decode one record body (`kind | payload`, without the length).
    pub fn decode(rec: &[u8]) -> Result<Decoded<ViewMsg>, MuxError> {
        let (&k, body) = rec.split_first().ok_or(MuxError::Length)?;
        let mut d = Dec(body);
        let m = match k {
            kind::HELLO => ViewMsg::Hello {
                major: d.u16()?,
                minor: d.u16()?,
                caps: d.strs(MAX_MUX_CAPS, MAX_MUX_STR)?,
                client: d.str(MAX_MUX_STR)?,
            },
            kind::ATTACH => ViewMsg::Attach,
            kind::LIST => ViewMsg::List,
            kind::INPUT => ViewMsg::Input {
                pane: d.u64()?,
                bytes: d.bytes(MAX_CHUNK)?.to_vec(),
            },
            kind::RESIZE => ViewMsg::Resize {
                pane: d.u64()?,
                rows: d.u16()?,
                cols: d.u16()?,
                cell_w: d.u16()?,
                cell_h: d.u16()?,
            },
            kind::SPAWN => {
                let token = d.u64()?;
                let argv = d.strs(MAX_SPAWN_ARGV, MAX_SPAWN_ELEM)?;
                let cwd = if d.bool()? {
                    Some(d.str(MAX_SPAWN_ELEM)?)
                } else {
                    None
                };
                ViewMsg::Spawn {
                    token,
                    argv,
                    cwd,
                    rows: d.u16()?,
                    cols: d.u16()?,
                }
            }
            kind::CLOSE => ViewMsg::Close { pane: d.u64()? },
            kind::RESYNC => ViewMsg::Resync { pane: d.u64()? },
            kind::DETACH => ViewMsg::Detach,
            kind::DETACH_ALL => ViewMsg::DetachAll,
            kind::KILL_SESSION => ViewMsg::KillSession,
            other => return Ok(Decoded::Skipped(other)),
        };
        d.end()?;
        Ok(Decoded::Msg(m))
    }
}

impl ServerMsg {
    /// The framed record.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e;
        match self {
            ServerMsg::Welcome {
                major,
                minor,
                caps,
                session,
                view,
            } => {
                e = Enc::new(kind::WELCOME);
                e.u16(*major).u16(*minor).strs(caps).str(session).u64(*view);
            }
            ServerMsg::Refused { reason } => {
                e = Enc::new(kind::REFUSED);
                e.str(reason);
            }
            ServerMsg::Panes(list) => {
                e = Enc::new(kind::PANES);
                e.u32(list.len() as u32);
                for p in list {
                    e.u64(p.pane)
                        .u16(p.rows)
                        .u16(p.cols)
                        .bool(p.alive)
                        .u32(p.views);
                }
            }
            ServerMsg::SnapBegin {
                pane,
                seq,
                read_only,
                state,
            } => {
                e = Enc::new(kind::SNAP_BEGIN);
                e.u64(*pane).u64(*seq).bool(*read_only).bytes(state);
            }
            ServerMsg::SnapPage {
                pane,
                screen,
                bytes,
            } => {
                e = Enc::new(kind::SNAP_PAGE);
                e.u64(*pane).u8(*screen).bytes(bytes);
            }
            ServerMsg::SnapEnd { pane } => {
                e = Enc::new(kind::SNAP_END);
                e.u64(*pane);
            }
            ServerMsg::Tee { pane, seq, bytes } => {
                e = Enc::new(kind::TEE);
                e.u64(*pane).u64(*seq).bytes(bytes);
            }
            ServerMsg::Resized {
                pane,
                seq,
                rows,
                cols,
            } => {
                e = Enc::new(kind::RESIZED);
                e.u64(*pane).u64(*seq).u16(*rows).u16(*cols);
            }
            ServerMsg::Spawned { token, pane } => {
                e = Enc::new(kind::SPAWNED);
                e.u64(*token).bool(pane.is_some()).u64(pane.unwrap_or(0));
            }
            ServerMsg::Exited { pane, status } => {
                e = Enc::new(kind::EXITED);
                e.u64(*pane)
                    .bool(status.is_some())
                    .u32(status.unwrap_or(0) as u32);
            }
        }
        e.done()
    }

    /// Decode one record body (`kind | payload`, without the length).
    pub fn decode(rec: &[u8]) -> Result<Decoded<ServerMsg>, MuxError> {
        let (&k, body) = rec.split_first().ok_or(MuxError::Length)?;
        let mut d = Dec(body);
        let m = match k {
            kind::WELCOME => ServerMsg::Welcome {
                major: d.u16()?,
                minor: d.u16()?,
                caps: d.strs(MAX_MUX_CAPS, MAX_MUX_STR)?,
                session: d.str(MAX_MUX_STR)?,
                view: d.u64()?,
            },
            kind::REFUSED => ServerMsg::Refused {
                reason: d.str(MAX_MUX_STR)?,
            },
            kind::PANES => {
                let n = d.u32()? as usize;
                if n > MAX_PANES {
                    return Err(MuxError::Malformed);
                }
                let mut v = Vec::with_capacity(n.min(d.0.len() / 17 + 1));
                for _ in 0..n {
                    v.push(PaneEntry {
                        pane: d.u64()?,
                        rows: d.u16()?,
                        cols: d.u16()?,
                        alive: d.bool()?,
                        views: d.u32()?,
                    });
                }
                ServerMsg::Panes(v)
            }
            kind::SNAP_BEGIN => ServerMsg::SnapBegin {
                pane: d.u64()?,
                seq: d.u64()?,
                read_only: d.bool()?,
                state: d.bytes(MAX_RECORD)?.to_vec(),
            },
            kind::SNAP_PAGE => ServerMsg::SnapPage {
                pane: d.u64()?,
                screen: match d.u8()? {
                    s @ (0 | 1) => s,
                    _ => return Err(MuxError::Malformed),
                },
                bytes: d.bytes(MAX_RECORD)?.to_vec(),
            },
            kind::SNAP_END => ServerMsg::SnapEnd { pane: d.u64()? },
            kind::TEE => ServerMsg::Tee {
                pane: d.u64()?,
                seq: d.u64()?,
                bytes: d.bytes(MAX_CHUNK)?.to_vec(),
            },
            kind::RESIZED => ServerMsg::Resized {
                pane: d.u64()?,
                seq: d.u64()?,
                rows: d.u16()?,
                cols: d.u16()?,
            },
            kind::SPAWNED => {
                let token = d.u64()?;
                let some = d.bool()?;
                let pane = d.u64()?;
                ServerMsg::Spawned {
                    token,
                    pane: some.then_some(pane),
                }
            }
            kind::EXITED => {
                let pane = d.u64()?;
                let some = d.bool()?;
                let st = d.u32()? as i32;
                ServerMsg::Exited {
                    pane,
                    status: some.then_some(st),
                }
            }
            other => return Ok(Decoded::Skipped(other)),
        };
        d.end()?;
        Ok(Decoded::Msg(m))
    }
}

struct Dec<'a>(&'a [u8]);

impl Dec<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], MuxError> {
        if self.0.len() < n {
            return Err(MuxError::Malformed);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, MuxError> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, MuxError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(MuxError::Malformed),
        }
    }
    fn u16(&mut self) -> Result<u16, MuxError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, MuxError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, MuxError> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(a))
    }
    fn bytes(&mut self, max: usize) -> Result<&[u8], MuxError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(MuxError::Malformed);
        }
        self.take(n)
    }
    fn str(&mut self, max: usize) -> Result<String, MuxError> {
        String::from_utf8(self.bytes(max)?.to_vec()).map_err(|_| MuxError::Malformed)
    }
    fn strs(&mut self, max_n: usize, max_len: usize) -> Result<Vec<String>, MuxError> {
        let n = self.u32()? as usize;
        if n > max_n {
            return Err(MuxError::Malformed);
        }
        let mut v = Vec::with_capacity(n.min(self.0.len() / 4 + 1));
        for _ in 0..n {
            v.push(self.str(max_len)?);
        }
        Ok(v)
    }
    fn end(&self) -> Result<(), MuxError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(MuxError::Malformed)
        }
    }
}

/// Splits a byte stream into records. Holds at most one record (≤ [`MAX_RECORD`]) plus a
/// read's worth; a bad length poisons it.
#[derive(Debug, Default)]
pub struct RecordDecoder {
    buf: Vec<u8>,
    start: usize,
    poisoned: bool,
    /// Largest record accepted; 0 = [`MAX_RECORD`].
    limit: usize,
}

impl RecordDecoder {
    /// An empty decoder.
    #[must_use]
    pub fn new() -> RecordDecoder {
        RecordDecoder::default()
    }

    /// A decoder that refuses records over `limit` bytes (the server caps what a view may
    /// make it buffer far below [`MAX_RECORD`]: views send input and spawns, not pages).
    #[must_use]
    pub fn with_limit(limit: usize) -> RecordDecoder {
        RecordDecoder {
            limit: limit.min(MAX_RECORD),
            ..RecordDecoder::default()
        }
    }

    /// Append received bytes.
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.poisoned {
            return;
        }
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start > 1 << 20 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete record body (`kind | payload`), if one has arrived.
    pub fn next_record(&mut self) -> Option<Result<&[u8], MuxError>> {
        if self.poisoned {
            return None;
        }
        let avail = &self.buf[self.start..];
        if avail.len() < 4 {
            return None;
        }
        let len = u32::from_le_bytes([avail[0], avail[1], avail[2], avail[3]]) as usize;
        let max = if self.limit == 0 {
            MAX_RECORD
        } else {
            self.limit
        };
        if len == 0 || len > max {
            self.poisoned = true;
            return Some(Err(MuxError::Length));
        }
        if avail.len() < 4 + len {
            return None;
        }
        let s = self.start + 4;
        self.start = s + len;
        Some(Ok(&self.buf[s..s + len]))
    }

    /// Bytes buffered and not yet returned.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.buf.len() - self.start
    }

    /// A bad length was seen; nothing more is decoded.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
}

/// The hello answer: `Ok` when the majors match, else the refusal text (T-21 §4).
pub fn check_version(major: u16, minor: u16) -> Result<(), String> {
    if major == MUX_MAJOR {
        Ok(())
    } else {
        Err(format!(
            "protocol version {major}.{minor} is not compatible with this server ({MUX_MAJOR}.{MUX_MINOR}); \
             use the same Apotheosis version for the server and its views"
        ))
    }
}

/// Body of the `mux_wire` fuzz target (T-18 §11): arbitrary bytes through the record
/// decoder and both message decoders never panic, the buffer stays bounded, and a
/// decoded message re-encodes to a record that decodes to itself.
pub fn fuzz_mux(data: &[u8]) {
    let mut dec = RecordDecoder::new();
    for chunk in data.chunks(61) {
        dec.feed(chunk);
        while let Some(rec) = dec.next_record() {
            let Ok(rec) = rec else { break };
            let rec = rec.to_vec();
            if let Ok(Decoded::Msg(m)) = ViewMsg::decode(&rec) {
                let enc = m.encode();
                assert_eq!(ViewMsg::decode(&enc[4..]), Ok(Decoded::Msg(m)));
            }
            if let Ok(Decoded::Msg(m)) = ServerMsg::decode(&rec) {
                let enc = m.encode();
                assert_eq!(ServerMsg::decode(&enc[4..]), Ok(Decoded::Msg(m)));
            }
        }
        assert!(dec.pending() <= MAX_RECORD + 4 + 61);
    }
    let _ = ViewMsg::decode(data);
    let _ = ServerMsg::decode(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_msgs() -> Vec<ViewMsg> {
        vec![
            ViewMsg::Hello {
                major: MUX_MAJOR,
                minor: MUX_MINOR,
                caps: vec!["x".into()],
                client: "apotheosis".into(),
            },
            ViewMsg::Attach,
            ViewMsg::List,
            ViewMsg::Input {
                pane: 3,
                bytes: b"ls\r".to_vec(),
            },
            ViewMsg::Resize {
                pane: 3,
                rows: 24,
                cols: 80,
                cell_w: 9,
                cell_h: 18,
            },
            ViewMsg::Spawn {
                token: 7,
                argv: vec!["sh".into(), "-c".into(), "true".into()],
                cwd: Some("/tmp".into()),
                rows: 10,
                cols: 20,
            },
            ViewMsg::Spawn {
                token: 8,
                argv: vec![],
                cwd: None,
                rows: 1,
                cols: 1,
            },
            ViewMsg::Close { pane: 1 },
            ViewMsg::Resync { pane: 2 },
            ViewMsg::Detach,
            ViewMsg::DetachAll,
            ViewMsg::KillSession,
        ]
    }

    fn server_msgs() -> Vec<ServerMsg> {
        vec![
            ServerMsg::Welcome {
                major: 1,
                minor: 0,
                caps: vec![],
                session: "default".into(),
                view: 9,
            },
            ServerMsg::Refused {
                reason: "no".into(),
            },
            ServerMsg::Panes(vec![PaneEntry {
                pane: 1,
                rows: 2,
                cols: 3,
                alive: true,
                views: 1,
            }]),
            ServerMsg::SnapBegin {
                pane: 1,
                seq: 5,
                read_only: true,
                state: vec![1, 2, 3],
            },
            ServerMsg::SnapPage {
                pane: 1,
                screen: 1,
                bytes: vec![0; 100],
            },
            ServerMsg::SnapEnd { pane: 1 },
            ServerMsg::Tee {
                pane: 1,
                seq: 6,
                bytes: b"out".to_vec(),
            },
            ServerMsg::Resized {
                pane: 1,
                seq: 7,
                rows: 5,
                cols: 6,
            },
            ServerMsg::Spawned {
                token: 7,
                pane: Some(4),
            },
            ServerMsg::Spawned {
                token: 8,
                pane: None,
            },
            ServerMsg::Exited {
                pane: 4,
                status: Some(-1),
            },
            ServerMsg::Exited {
                pane: 4,
                status: None,
            },
        ]
    }

    #[test]
    fn every_message_round_trips_in_any_chunking() {
        let mut stream = Vec::new();
        for m in view_msgs() {
            stream.extend(m.encode());
        }
        for step in [1, 2, 7, 4096] {
            let mut d = RecordDecoder::new();
            let mut got = Vec::new();
            for c in stream.chunks(step) {
                d.feed(c);
                while let Some(r) = d.next_record() {
                    got.push(ViewMsg::decode(r.unwrap()).unwrap());
                }
            }
            let want: Vec<_> = view_msgs().into_iter().map(Decoded::Msg).collect();
            assert_eq!(got, want);
        }
        for m in server_msgs() {
            let e = m.encode();
            assert_eq!(
                u32::from_le_bytes([e[0], e[1], e[2], e[3]]) as usize,
                e.len() - 4
            );
            assert_eq!(ServerMsg::decode(&e[4..]), Ok(Decoded::Msg(m)));
        }
    }

    #[test]
    fn golden_hello_bytes() {
        let e = ViewMsg::Hello {
            major: 1,
            minor: 0,
            caps: vec![],
            client: "v".into(),
        }
        .encode();
        assert_eq!(
            e,
            [14, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, b'v']
        );
    }

    #[test]
    fn unknown_kinds_are_skipped_and_bad_records_refused() {
        assert_eq!(ViewMsg::decode(&[200, 1, 2]), Ok(Decoded::Skipped(200)));
        assert_eq!(ServerMsg::decode(&[3]), Ok(Decoded::Skipped(3)));
        assert_eq!(ViewMsg::decode(&[]), Err(MuxError::Length));
        // Trailing bytes, truncation, a bad bool, an oversize chunk.
        assert_eq!(ViewMsg::decode(&[2, 0]), Err(MuxError::Malformed));
        assert_eq!(ViewMsg::decode(&[7, 1, 2]), Err(MuxError::Malformed));
        let mut bad = ServerMsg::SnapBegin {
            pane: 1,
            seq: 1,
            read_only: false,
            state: vec![],
        }
        .encode();
        bad[4 + 16 + 1] = 2;
        assert_eq!(ServerMsg::decode(&bad[4..]), Err(MuxError::Malformed));
        let mut big = Enc::new(kind::INPUT);
        big.u64(1).u32((MAX_CHUNK + 1) as u32);
        big.0.extend(vec![0; MAX_CHUNK + 1]);
        assert_eq!(ViewMsg::decode(&big.done()[4..]), Err(MuxError::Malformed));
        let mut d = RecordDecoder::new();
        d.feed(&[0, 0, 0, 0]);
        assert_eq!(d.next_record(), Some(Err(MuxError::Length)));
        assert!(d.is_poisoned());
        let mut d = RecordDecoder::new();
        d.feed(&((MAX_RECORD + 1) as u32).to_le_bytes());
        assert_eq!(d.next_record(), Some(Err(MuxError::Length)));
        // A capped decoder refuses at its cap, before buffering the body.
        let mut d = RecordDecoder::with_limit(16);
        d.feed(&ViewMsg::Detach.encode());
        assert_eq!(d.next_record(), Some(Ok(&[kind::DETACH][..])));
        d.feed(&17u32.to_le_bytes());
        assert_eq!(d.next_record(), Some(Err(MuxError::Length)));
    }

    #[test]
    fn version_skew_is_refused_with_a_message() {
        assert!(check_version(MUX_MAJOR, 99).is_ok());
        let e = check_version(MUX_MAJOR + 1, 0).unwrap_err();
        assert!(e.contains("not compatible"));
    }

    #[test]
    fn the_seed_corpus_decodes_and_holds() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fuzz/corpus/mux_wire");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let data = std::fs::read(e.unwrap().path()).unwrap();
            fuzz_mux(&data);
            n += 1;
        }
        assert!(n >= 8);
        // The well-formed seeds are what they claim: every record decodes.
        let hello = std::fs::read(format!("{dir}/seed-1.bin")).unwrap();
        let mut d = RecordDecoder::new();
        d.feed(&hello);
        let r = d.next_record().unwrap().unwrap().to_vec();
        assert!(matches!(
            ViewMsg::decode(&r),
            Ok(Decoded::Msg(ViewMsg::Hello { major: 1, .. }))
        ));
        for f in [
            "seed-3.bin",
            "seed-4.bin",
            "seed-5.bin",
            "seed-7.bin",
            "seed-8.bin",
        ] {
            let data = std::fs::read(format!("{dir}/{f}")).unwrap();
            let mut d = RecordDecoder::new();
            d.feed(&data);
            while let Some(r) = d.next_record() {
                let r = r.unwrap().to_vec();
                let ok = if r[0] < 64 {
                    matches!(ViewMsg::decode(&r), Ok(Decoded::Msg(_)))
                } else {
                    matches!(ServerMsg::decode(&r), Ok(Decoded::Msg(_)))
                };
                assert!(ok, "{f}: kind {}", r[0]);
            }
        }
    }

    #[test]
    fn fuzz_body_holds_on_seeds() {
        let mut all = Vec::new();
        for m in view_msgs() {
            all.extend(m.encode());
        }
        for m in server_msgs() {
            all.extend(m.encode());
        }
        fuzz_mux(&all);
        for i in 0..all.len() {
            let mut c = all.clone();
            c[i] ^= 0xff;
            fuzz_mux(&c);
            fuzz_mux(&all[..i]);
        }
    }
}
