// SPDX-License-Identifier: AGPL-3.0-only
//! The S-04 §4 audit store: an append-only, hash-chained journal on disk.
//!
//! One store serves every durable thing `policyd` knows. The task/counter
//! journal is not a second format living beside this one — it is the `task`
//! record kind inside it (ADR 0046). That is the point: a reader who can
//! verify the chain can verify *everything*, and there is no second file
//! whose disagreement with this one has to be adjudicated later.
//!
//! Three properties are deliberate.
//!
//! * **The chain is over canonical bytes.** `hash = BLAKE3(prev_hash ||
//!   canonical(record sans hash))`, and the encoder emits exactly one byte
//!   string for a given record (ADR 0044). A re-encode that produced
//!   different bytes would make an honest log look forged, so the encoding is
//!   pinned as hard as the hash is.
//! * **A truncated tail is repaired; an invalid tail is not.** Losing power
//!   mid-append is normal and the last partial frame is simply dropped. A
//!   frame that is *complete* but does not chain is a different event
//!   entirely — it is moved aside intact rather than deleted, because it is
//!   evidence.
//! * **The chain does not restart at a segment boundary.** `seq` and
//!   `prev_hash` continue across a rotation, so replay reads the sealed
//!   segments before the open one. A store that replayed only its open
//!   segment would forget every task it had ever opened the moment it
//!   rotated, and would then reject its own next record as a chain break.
//! * **Durability is bounded, not per-record.** fsync on segment close and
//!   every [`FSYNC_INTERVAL_MS`]; callers who need an answer to be durable
//!   before it is spoken call [`Store::sync`] themselves. The task store does
//!   exactly that — journal before answer (A-04 §4).

use policy_eval::cbor::{enc, MapBuilder, Reader, Writer};
use policy_eval::{cbor, Ulid};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotate once a segment holds this many *uncompressed* bytes (ADR 0046).
/// Uncompressed, because the threshold is about how much a reader must replay
/// to reach the tail, and compression ratio varies with content.
pub const SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
/// Upper bound on how long an already-written record may sit unsynced.
pub const FSYNC_INTERVAL_MS: u64 = 250;
/// Refuse a frame larger than this. A length prefix is the one field a
/// corrupt tail can turn into an allocation request, so it is bounded.
const MAX_FRAME: u32 = 4 * 1024 * 1024;

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// What a record is about. Milestone 10 emits `task` and `anchor`; the
/// remaining kinds arrive with milestone 12 and change nothing here, which is
/// the reason the envelope was built first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Task,
    Anchor,
    GrantIssued,
    GrantRevoked,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Task => "task",
            Kind::Anchor => "anchor",
            Kind::GrantIssued => "grant_issued",
            Kind::GrantRevoked => "grant_revoked",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "task" => Kind::Task,
            "anchor" => Kind::Anchor,
            "grant_issued" => Kind::GrantIssued,
            "grant_revoked" => Kind::GrantRevoked,
            _ => return None,
        })
    }
}

/// The S-04 §1 envelope.
///
/// The optional fields are *absent* when they do not apply, never zeroed.
/// That is normative and it matters: a zeroed `grant_id` is a valid-looking
/// grant id, and a record that claims one is a record that lies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub seq: u64,
    /// Wall-clock milliseconds since the epoch — what a human reads.
    pub ts: u64,
    /// Monotonic milliseconds since store open — what an auditor trusts when
    /// the wall clock steps.
    pub mono: u64,
    pub kind: Kind,
    pub principal: String,
    pub grant_id: Option<Ulid>,
    pub task_id: Option<Ulid>,
    pub chain_id: Option<String>,
    pub req_id: Option<u64>,
    pub serial: Option<u64>,
    /// Kind-specific body, itself canonical CBOR.
    pub body: Vec<u8>,
    pub prev_hash: [u8; 32],
    pub hash: [u8; 32],
}

impl Record {
    /// The bytes the chain hashes: the record without its own `hash`.
    fn unhashed(&self) -> Vec<u8> {
        let mut m = MapBuilder::new();
        m.insert("body", self.body.clone());
        m.insert_opt("chain_id", self.chain_id.as_ref().map(|c| enc(|w| w.text(c))));
        m.insert_opt("grant_id", self.grant_id.map(|g| enc(|w| w.bytes(&g.0))));
        m.insert("kind", enc(|w| w.text(self.kind.as_str())));
        m.insert("mono", enc(|w| w.u64(self.mono)));
        m.insert("prev_hash", enc(|w| w.bytes(&self.prev_hash)));
        m.insert("principal", enc(|w| w.text(&self.principal)));
        m.insert_opt("req_id", self.req_id.map(|r| enc(|w| w.u64(r))));
        m.insert("seq", enc(|w| w.u64(self.seq)));
        m.insert_opt("serial", self.serial.map(|s| enc(|w| w.u64(s))));
        m.insert_opt("task_id", self.task_id.map(|t| enc(|w| w.bytes(&t.0))));
        m.insert("ts", enc(|w| w.u64(self.ts)));
        m.finish()
    }

    /// The hash this record *should* carry, given its contents.
    pub fn compute_hash(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(&self.prev_hash);
        h.update(&self.unhashed());
        *h.finalize().as_bytes()
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.array(2);
        w.raw(&self.unhashed());
        w.bytes(&self.hash);
        w.finish()
    }

    fn decode(bytes: &[u8]) -> cbor::Result<Record> {
        let mut r = Reader::new(bytes);
        if r.array_len()? != 2 {
            return Err(cbor::Error::Type);
        }
        let start = r.position();
        let n = r.map_begin()?;
        let mut rec = Record {
            seq: 0,
            ts: 0,
            mono: 0,
            kind: Kind::Anchor,
            principal: String::new(),
            grant_id: None,
            task_id: None,
            chain_id: None,
            req_id: None,
            serial: None,
            body: Vec::new(),
            prev_hash: [0; 32],
            hash: [0; 32],
        };
        let mut seen_kind = false;
        for _ in 0..n {
            match r.key()? {
                "body" => {
                    let at = r.position();
                    r.skip()?;
                    rec.body = bytes[at..r.position()].to_vec();
                }
                "chain_id" => rec.chain_id = Some(r.text()?.to_owned()),
                "grant_id" => rec.grant_id = Some(Ulid(r.byte_array::<16>()?)),
                "kind" => {
                    rec.kind = Kind::parse(r.text()?).ok_or(cbor::Error::Type)?;
                    seen_kind = true;
                }
                "mono" => rec.mono = r.u64()?,
                "prev_hash" => rec.prev_hash = r.byte_array::<32>()?,
                "principal" => rec.principal = r.text()?.to_owned(),
                "req_id" => rec.req_id = Some(r.u64()?),
                "seq" => rec.seq = r.u64()?,
                "serial" => rec.serial = Some(r.u64()?),
                "task_id" => rec.task_id = Some(Ulid(r.byte_array::<16>()?)),
                "ts" => rec.ts = r.u64()?,
                _ => return Err(cbor::Error::Type),
            }
        }
        r.map_end()?;
        let _ = start;
        rec.hash = r.byte_array::<32>()?;
        r.finish()?;
        if !seen_kind {
            return Err(cbor::Error::Type);
        }
        Ok(rec)
    }
}

/// Why a store could not be opened or appended to.
#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Cbor(cbor::Error),
    /// A complete frame whose hash does not continue the chain. Kept as its
    /// own variant because the response is quarantine, not truncation.
    ChainBroken {
        seq: u64,
    },
    /// A single record too large to frame.
    TooLarge,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "audit store io: {e}"),
            StoreError::Cbor(e) => write!(f, "audit store decode: {e}"),
            StoreError::ChainBroken { seq } => write!(f, "audit chain breaks at seq {seq}"),
            StoreError::TooLarge => f.write_str("audit record exceeds the frame limit"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<cbor::Error> for StoreError {
    fn from(e: cbor::Error) -> Self {
        StoreError::Cbor(e)
    }
}

type Result<T> = std::result::Result<T, StoreError>;

/// The genesis link: the chain starts from all-zero, and only there. Any
/// later record carrying a zero `prev_hash` is a record claiming to be first.
pub const GENESIS: [u8; 32] = [0; 32];

/// An append-only hash-chained store rooted at one directory.
pub struct Store {
    dir: PathBuf,
    open_path: PathBuf,
    file: File,
    written: u64,
    seq: u64,
    head: [u8; 32],
    last_sync_ms: u64,
    opened_at: SystemTime,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Store {
    /// Opens (creating if needed) the store at `dir`, replaying the open
    /// segment to recover `seq` and the chain head.
    ///
    /// Replay is not optional and there is no "trust the last state file"
    /// shortcut: the chain head is the only thing that proves the next record
    /// continues this log rather than a forked one.
    pub fn open(dir: &Path) -> Result<Store> {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE))?;
        let open_path = dir.join("current.open");
        let (seq, head, written) = Store::replay(dir, &open_path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(FILE_MODE)
            .open(&open_path)?;
        Ok(Store {
            dir: dir.to_path_buf(),
            open_path,
            file,
            written,
            seq,
            head,
            last_sync_ms: now_ms(),
            opened_at: SystemTime::now(),
        })
    }

    /// Reads every framed record in the store, sealed segments first, then
    /// the open one, verifying the chain the whole way.
    ///
    /// Returns the next sequence number, the chain head, and the length of
    /// the valid prefix of the *open* segment. A truncated trailing frame in
    /// the open segment is repaired by truncating the file to that prefix; a
    /// complete frame that breaks the chain is moved aside and reported. A
    /// sealed segment is never repaired — it is finished, so anything wrong
    /// with it is a broken chain and nothing else.
    fn replay(dir: &Path, path: &Path) -> Result<(u64, [u8; 32], u64)> {
        let mut head = GENESIS;
        let mut seq = 0u64;
        for sealed in Store::sealed_segments(dir)? {
            let buf = Store::read_sealed(&sealed)?;
            let mut pos = 0usize;
            while pos + 4 <= buf.len() {
                let len = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
                if len == 0 || len > MAX_FRAME {
                    return Err(StoreError::ChainBroken { seq });
                }
                let end = pos + 4 + len as usize;
                if end > buf.len() {
                    return Err(StoreError::ChainBroken { seq });
                }
                let rec = Record::decode(&buf[pos + 4..end]).map_err(|_| StoreError::ChainBroken { seq })?;
                if rec.prev_hash != head || rec.hash != rec.compute_hash() || rec.seq != seq {
                    return Err(StoreError::ChainBroken { seq: rec.seq });
                }
                head = rec.hash;
                seq = rec.seq + 1;
                pos = end;
            }
        }
        let mut buf = Vec::new();
        match File::open(path) {
            Ok(mut f) => {
                f.read_to_end(&mut buf)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((seq, head, 0));
            }
            Err(e) => return Err(e.into()),
        }
        let mut pos = 0usize;
        while pos + 4 <= buf.len() {
            let len = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            if len == 0 || len > MAX_FRAME {
                break;
            }
            let end = pos + 4 + len as usize;
            if end > buf.len() {
                break; // truncated tail: repaired below
            }
            let rec = match Record::decode(&buf[pos + 4..end]) {
                Ok(r) => r,
                // An undecodable *complete* frame is not a torn write; it is
                // a corrupt one. Treat it exactly like a chain break.
                Err(_) => {
                    Store::quarantine(path, pos as u64)?;
                    return Err(StoreError::ChainBroken { seq });
                }
            };
            if rec.prev_hash != head || rec.hash != rec.compute_hash() || rec.seq != seq {
                Store::quarantine(path, pos as u64)?;
                return Err(StoreError::ChainBroken { seq: rec.seq });
            }
            head = rec.hash;
            seq = rec.seq + 1;
            pos = end;
        }
        if pos as u64 != buf.len() as u64 {
            // Drop the torn tail. Everything before it verified.
            let f = OpenOptions::new().write(true).open(path)?;
            f.set_len(pos as u64)?;
            f.sync_all()?;
        }
        Ok((seq, head, pos as u64))
    }

    /// Moves the segment aside, keeping it byte-for-byte, and leaves the
    /// verified prefix in place as the new open segment.
    fn quarantine(path: &Path, valid_len: u64) -> Result<()> {
        let mut aside = path.to_path_buf();
        aside.set_extension(format!("quarantine.{}", now_ms()));
        fs::copy(path, &aside)?;
        fs::set_permissions(&aside, fs::Permissions::from_mode(FILE_MODE))?;
        let f = OpenOptions::new().write(true).open(path)?;
        f.set_len(valid_len)?;
        f.sync_all()?;
        Ok(())
    }

    /// The current chain head — what the next record will name as `prev_hash`.
    pub fn head(&self) -> [u8; 32] {
        self.head
    }

    /// The sequence number the next appended record will carry.
    pub fn next_seq(&self) -> u64 {
        self.seq
    }

    /// Appends one record, filling in `seq`, `ts`, `mono`, `prev_hash` and
    /// `hash` from the store's own state.
    ///
    /// The caller supplies what a record is *about*; it never supplies its
    /// position in the chain, because a caller that could choose its own
    /// `seq` could rewrite history by choosing an old one.
    #[allow(clippy::too_many_arguments)]
    pub fn append(&mut self, mut rec: Record) -> Result<Record> {
        rec.seq = self.seq;
        rec.ts = now_ms();
        rec.mono = self
            .opened_at
            .elapsed()
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        rec.prev_hash = self.head;
        rec.hash = rec.compute_hash();

        let bytes = rec.encode();
        let len = u32::try_from(bytes.len()).map_err(|_| StoreError::TooLarge)?;
        if len > MAX_FRAME {
            return Err(StoreError::TooLarge);
        }
        self.file.write_all(&len.to_be_bytes())?;
        self.file.write_all(&bytes)?;
        self.written += 4 + bytes.len() as u64;
        self.head = rec.hash;
        self.seq += 1;

        let now = now_ms();
        if now.saturating_sub(self.last_sync_ms) >= FSYNC_INTERVAL_MS {
            self.sync()?;
        }
        if self.written >= SEGMENT_BYTES {
            self.rotate()?;
        }
        Ok(rec)
    }

    /// Forces everything written so far to durable storage. The task store
    /// calls this before answering, which is what makes "journal before
    /// answer" true rather than aspirational.
    pub fn sync(&mut self) -> Result<()> {
        self.file.sync_data()?;
        self.last_sync_ms = now_ms();
        Ok(())
    }

    /// Seals the open segment into one zstd frame and starts a fresh one.
    ///
    /// The sealed name is `<YYYYMMDD>-<nnn>.seg` (ADR 0046). The counter is
    /// per day and padded so that a plain lexicographic listing is also a
    /// chronological one.
    pub fn rotate(&mut self) -> Result<()> {
        self.sync()?;
        let date = utc_date(now_ms());
        let mut n = 0u32;
        let sealed = loop {
            let p = self.dir.join(format!("{date}-{n:04}.seg"));
            if !p.exists() {
                break p;
            }
            n += 1;
        };
        let mut plain = Vec::new();
        File::open(&self.open_path)?.read_to_end(&mut plain)?;
        let compressed = zstd::encode_all(&plain[..], 3)?;
        let mut out = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(FILE_MODE)
            .open(&sealed)?;
        out.write_all(&compressed)?;
        out.sync_all()?;

        let f = OpenOptions::new().write(true).open(&self.open_path)?;
        f.set_len(0)?;
        f.sync_all()?;
        self.file = OpenOptions::new()
            .append(true)
            .mode(FILE_MODE)
            .open(&self.open_path)?;
        self.written = 0;

        // The first record of a new segment anchors it to the previous one,
        // so a reader holding only this segment can still say what it
        // continues from.
        let anchor = Record {
            seq: 0,
            ts: 0,
            mono: 0,
            kind: Kind::Anchor,
            principal: "policyd".into(),
            grant_id: None,
            task_id: None,
            chain_id: None,
            req_id: None,
            serial: None,
            body: anchor_body(&sealed, self.head),
            prev_hash: GENESIS,
            hash: GENESIS,
        };
        self.append(anchor)?;
        Ok(())
    }

    /// Replays the whole store — sealed segments oldest-first, then the open
    /// one — handing every record to `f`. Used at startup to rebuild the task
    /// store from its journal.
    ///
    /// Sealed segments are included because a task outlives a rotation: a
    /// replay of the open segment alone would forget open tasks and their
    /// spent counters, which is precisely the restart-laundering A-04 §6
    /// forbids.
    pub fn for_each(&self, mut f: impl FnMut(&Record)) -> Result<()> {
        for sealed in Store::sealed_segments(&self.dir)? {
            let buf = Store::read_sealed(&sealed)?;
            Store::for_each_frame(&buf, &mut f)?;
        }
        let mut buf = Vec::new();
        File::open(&self.open_path)?.read_to_end(&mut buf)?;
        Store::for_each_frame(&buf, &mut f)
    }

    fn for_each_frame(buf: &[u8], f: &mut impl FnMut(&Record)) -> Result<()> {
        let mut pos = 0usize;
        while pos + 4 <= buf.len() {
            let len = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
            let end = pos + 4 + len;
            if len == 0 || end > buf.len() {
                break;
            }
            f(&Record::decode(&buf[pos + 4..end])?);
            pos = end;
        }
        Ok(())
    }

    /// The sealed `.seg` files in chain order.
    ///
    /// The name carries the date and a zero-padded per-day counter (ADR
    /// 0046) exactly so that sorting the names sorts the segments; nothing
    /// here has to open a file to find out what order the log is in.
    fn sealed_segments(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "seg") {
                out.push(path);
            }
        }
        out.sort();
        Ok(out)
    }

    fn read_sealed(path: &Path) -> Result<Vec<u8>> {
        let mut compressed = Vec::new();
        File::open(path)?.read_to_end(&mut compressed)?;
        zstd::decode_all(&compressed[..]).map_err(StoreError::from)
    }
}

fn anchor_body(sealed: &Path, head: [u8; 32]) -> Vec<u8> {
    let mut m = MapBuilder::new();
    m.insert("head", enc(|w| w.bytes(&head)));
    m.insert(
        "sealed",
        enc(|w| w.text(&sealed.file_name().unwrap_or_default().to_string_lossy())),
    );
    m.finish()
}

/// UTC `YYYYMMDD` from epoch milliseconds.
///
/// Written out rather than pulled in: the only calendar fact a segment name
/// needs is the civil date, and the algorithm for that is short enough to
/// read. Proleptic Gregorian, via the days-from-civil inverse.
pub fn utc_date(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("policyd-audit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn rec(kind: Kind, principal: &str) -> Record {
        Record {
            seq: 0,
            ts: 0,
            mono: 0,
            kind,
            principal: principal.into(),
            grant_id: None,
            task_id: None,
            chain_id: None,
            req_id: None,
            serial: None,
            body: enc(|w| w.u64(1)),
            prev_hash: GENESIS,
            hash: GENESIS,
        }
    }

    #[test]
    fn the_chain_links_and_survives_a_reopen() {
        let dir = tmp("reopen");
        let mut s = Store::open(&dir).unwrap();
        let a = s.append(rec(Kind::Task, "agent:a")).unwrap();
        let b = s.append(rec(Kind::Task, "agent:b")).unwrap();
        assert_eq!(b.prev_hash, a.hash);
        s.sync().unwrap();
        drop(s);

        let s = Store::open(&dir).unwrap();
        assert_eq!(s.next_seq(), 2);
        assert_eq!(s.head(), b.hash);
        let mut seen = Vec::new();
        s.for_each(|r| seen.push(r.clone())).unwrap();
        assert_eq!(seen, vec![a, b]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_torn_tail_is_repaired_not_rejected() {
        let dir = tmp("torn");
        let mut s = Store::open(&dir).unwrap();
        let a = s.append(rec(Kind::Task, "agent:a")).unwrap();
        s.append(rec(Kind::Task, "agent:b")).unwrap();
        s.sync().unwrap();
        drop(s);

        // Chop the last frame in half, as a power cut would.
        let p = dir.join("current.open");
        let len = fs::metadata(&p).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(len - 5)
            .unwrap();

        let s = Store::open(&dir).unwrap();
        assert_eq!(s.next_seq(), 1);
        assert_eq!(s.head(), a.hash);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_tampered_record_breaks_the_chain_and_is_quarantined() {
        let dir = tmp("tamper");
        let mut s = Store::open(&dir).unwrap();
        s.append(rec(Kind::Task, "agent:a")).unwrap();
        s.append(rec(Kind::Task, "agent:b")).unwrap();
        s.sync().unwrap();
        drop(s);

        let p = dir.join("current.open");
        let mut bytes = fs::read(&p).unwrap();
        let n = bytes.len();
        bytes[n - 40] ^= 0xff; // inside the second record's body region
        fs::write(&p, &bytes).unwrap();

        assert!(matches!(Store::open(&dir), Err(StoreError::ChainBroken { .. })));
        assert!(fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("quarantine")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rotation_seals_a_zstd_segment_and_anchors_the_next() {
        let dir = tmp("rotate");
        let mut s = Store::open(&dir).unwrap();
        let a = s.append(rec(Kind::Task, "agent:a")).unwrap();
        s.rotate().unwrap();
        assert_eq!(s.head(), s.head());
        let sealed: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".seg"))
            .collect();
        assert_eq!(sealed.len(), 1);
        let raw = fs::read(sealed[0].path()).unwrap();
        let plain = zstd::decode_all(&raw[..]).unwrap();
        assert!(plain.windows(32).any(|w| w == a.hash));

        // The sealed segment is still part of the log: a replay that skipped
        // it would forget every task opened before the rotation.
        let mut seen = Vec::new();
        s.for_each(|r| seen.push(r.kind)).unwrap();
        assert_eq!(seen, vec![Kind::Task, Kind::Anchor]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_rotated_store_still_opens_and_keeps_its_chain() {
        let dir = tmp("rotate-reopen");
        let mut s = Store::open(&dir).unwrap();
        s.append(rec(Kind::Task, "agent:a")).unwrap();
        s.rotate().unwrap();
        let after = s.append(rec(Kind::Task, "agent:a")).unwrap();
        drop(s);

        // `seq` and `prev_hash` run across the segment boundary, so reopening
        // has to read the sealed segment to know where the chain is. A store
        // that restarted at GENESIS here would quarantine its own anchor.
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.head(), after.hash);
        assert_eq!(s.next_seq(), after.seq + 1);
        let mut seen = Vec::new();
        s.for_each(|r| seen.push(r.kind)).unwrap();
        assert_eq!(seen, vec![Kind::Task, Kind::Anchor, Kind::Task]);
        assert!(!fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("quarantine")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn permissions_are_owner_only() {
        let dir = tmp("perms");
        let mut s = Store::open(&dir).unwrap();
        s.append(rec(Kind::Task, "agent:a")).unwrap();
        s.sync().unwrap();
        let dm = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        let fm = fs::metadata(dir.join("current.open"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dm, DIR_MODE);
        assert_eq!(fm, FILE_MODE);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn utc_date_matches_known_days() {
        assert_eq!(utc_date(0), "19700101");
        assert_eq!(utc_date(1_700_000_000_000), "20231114");
    }
}
