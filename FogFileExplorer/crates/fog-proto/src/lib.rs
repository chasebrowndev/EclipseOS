// SPDX-License-Identifier: Apache-2.0

//! Fog's IPC wire types and framing (FOG §Architecture/IPC). No filesystem I/O.
//!
//! A frame is a little-endian `u32` payload length followed by the postcard
//! encoding of an [`Envelope`]. Every frame carries [`VERSION`]; a peer with a
//! different major version (or, while major is 0, a different minor) is
//! refused, see [`compatible`]. Paths and names are raw bytes, never
//! assumed to be UTF-8.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol version carried in every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

/// The version this build speaks.
pub const VERSION: Version = Version { major: 0, minor: 2 };

/// Largest payload accepted or produced, in bytes.
pub const MAX_FRAME: u32 = 64 << 20;

/// Every frame's payload: the version header, then the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub version: Version,
    pub body: T,
}

/// Entry type, from `d_type` (phase 1) or `statx` (phase 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
    Unknown,
}

/// One directory entry. `name` is the raw file name bytes.
///
/// Phase 1 (`getdents64`) fills only `name` and `kind`; `size`, `mtime_ns`
/// and `mode` stay `None` until phase-2 `statx` fills them through a
/// `DirDiff`'s `changed` (FOG §Performance model).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Entry {
    pub name: Vec<u8>,
    pub kind: Kind,
    pub size: Option<u64>,
    pub mtime_ns: Option<i128>,
    /// `st_mode`: file type and permission bits.
    pub mode: Option<u32>,
}

impl Entry {
    /// A phase-1 entry: name and kind, no metadata yet.
    pub fn new(name: Vec<u8>, kind: Kind) -> Self {
        Self {
            name,
            kind,
            size: None,
            mtime_ns: None,
            mode: None,
        }
    }

    /// The name for display: lossy UTF-8.
    pub fn display(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }
}

/// Daemon-assigned job id, unique for the life of `fogd`.
pub type JobId = u64;

/// What a job does on a name collision (FOG §File operations/Conflicts).
/// `Ask` pauses the job in [`JobStatus::Conflict`]; headless callers must
/// pick one of the others, and `Fail` is their default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ConflictPolicy {
    Ask,
    Skip,
    /// Keep both: the incoming file becomes `name (2).ext`.
    Rename,
    Replace,
    #[default]
    Fail,
}

/// A mutation for `fogd`'s job queue (FOG §File operations/Queue). Every
/// file change is one of these, whoever asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobSpec {
    /// Copy `srcs` into the directory `dest`.
    Copy {
        srcs: Vec<Vec<u8>>,
        dest: Vec<u8>,
        on_conflict: ConflictPolicy,
    },
    /// Move `srcs` into the directory `dest`.
    Move {
        srcs: Vec<Vec<u8>>,
        dest: Vec<u8>,
        on_conflict: ConflictPolicy,
    },
    /// Rename in place; `new_name` is a single path component.
    Rename {
        path: Vec<u8>,
        new_name: Vec<u8>,
        on_conflict: ConflictPolicy,
    },
    Trash {
        paths: Vec<Vec<u8>>,
        on_conflict: ConflictPolicy,
    },
    /// Permanent delete: never undoable (FOG §File operations/Undo journal).
    Delete {
        paths: Vec<Vec<u8>>,
        on_conflict: ConflictPolicy,
    },
    Mkdir {
        path: Vec<u8>,
        on_conflict: ConflictPolicy,
    },
    CreateFile {
        path: Vec<u8>,
        on_conflict: ConflictPolicy,
    },
    /// Restore [`TrashItem`]s, by `id`, to their original paths.
    Restore {
        trash_ids: Vec<Vec<u8>>,
        on_conflict: ConflictPolicy,
    },
}

/// Answer to a job paused in [`JobStatus::Conflict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Resolution {
    Replace,
    Skip,
    /// `name (2).ext`.
    KeepBoth,
    /// Directories only.
    Merge,
}

/// Control of a queued or running job. Pause and cancel take effect between
/// chunks and between files (FOG §File operations/Queue).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobAction {
    Pause,
    Resume,
    Cancel,
    /// `apply_all` answers every later conflict in the job the same way.
    Resolve {
        choice: Resolution,
        apply_all: bool,
    },
}

/// Job lifecycle (FOG §File operations state diagram).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Queued,
    Running,
    Paused,
    /// Waiting on [`JobAction::Resolve`]: `dest` already exists.
    Conflict {
        src: Vec<u8>,
        dest: Vec<u8>,
    },
    Done,
    Failed {
        errno: i32,
        msg: String,
    },
    Cancelled,
}

/// One item in the freedesktop trash (FOG §Filesystem backend/Trash).
/// `id` is opaque to clients; it names the item in `JobSpec::Restore`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrashItem {
    pub id: Vec<u8>,
    pub original_path: Vec<u8>,
    /// `DeletionDate` from the `.trashinfo`, as Unix seconds.
    pub deleted_s: Option<i64>,
    pub kind: Kind,
    pub size: Option<u64>,
}

/// Where a [`Place`] comes from (FOG §Desktop interop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlaceKind {
    Home,
    /// An XDG user dir (Documents, Downloads, ...).
    UserDir,
    /// `~/.config/gtk-3.0/bookmarks`.
    Bookmark,
    /// `recently-used.xbel`.
    Recent,
    Trash,
    /// A udisks2 mount.
    Mount,
}

/// A sidebar place.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Place {
    pub kind: PlaceKind,
    pub label: String,
    pub path: Vec<u8>,
}

/// Client to `fogd`. Paths are absolute, raw bytes.
///
/// `Subscribe` is answered like `ListDir`, then `fogd` keeps pushing
/// `DirDiff`s for that listing's `dir` until `Unsubscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    ListDir {
        path: Vec<u8>,
    },
    Stat {
        path: Vec<u8>,
    },
    Subscribe {
        path: Vec<u8>,
    },
    Unsubscribe {
        dir: u64,
    },
    /// Answered with `JobAccepted`, then `JobProgress` and `JobState` pushes.
    Job(JobSpec),
    JobControl {
        id: JobId,
        action: JobAction,
    },
    /// Undo the newest journal entry; answered with `UndoResult`.
    Undo,
    /// Open with `app` (a `.desktop` id), else the `mimeapps.list` default.
    Open {
        path: Vec<u8>,
        app: Option<String>,
    },
    ListTrash,
    Places,
}

/// `fogd` to client.
///
/// `order` is the display order as indices into the entry list. After a
/// `DirDiff`, indices refer to the list produced by [`apply_diff`].
/// `complete == false` means more entries follow as `DirDiff`s. A diff's
/// `changed` carries new metadata for entries already held (matched by
/// name); it never moves an index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    DirSnapshot {
        path: Vec<u8>,
        dir: u64,
        generation: u64,
        entries: Vec<Entry>,
        order: Vec<u32>,
        complete: bool,
    },
    DirDiff {
        dir: u64,
        generation: u64,
        removed: Vec<Vec<u8>>,
        added: Vec<Entry>,
        changed: Vec<Entry>,
        order: Vec<u32>,
        complete: bool,
    },
    Stat(StatReply),
    Error {
        path: Vec<u8>,
        errno: i32,
    },
    JobAccepted {
        id: JobId,
    },
    /// `current` is the path being worked on.
    JobProgress {
        id: JobId,
        bytes_done: u64,
        bytes_total: u64,
        files_done: u64,
        files_total: u64,
        current: Vec<u8>,
    },
    JobState {
        id: JobId,
        state: JobStatus,
    },
    /// `reason` explains a refused undo (FOG §File operations/Undo journal).
    UndoResult {
        ok: bool,
        reason: Option<String>,
    },
    TrashList(Vec<TrashItem>),
    PlacesList(Vec<Place>),
    /// `fog.kdl` was rejected and the previous valid config stays active
    /// (FOG §Configuration). `line` and `col` are 1-based.
    ConfigError {
        line: u32,
        col: u32,
        msg: String,
    },
    /// `Open` launched a handler for `path`.
    Opened {
        path: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatReply {
    pub path: Vec<u8>,
    pub kind: Kind,
    pub size: u64,
    pub mtime_ns: i128,
    pub inode: u64,
}

/// Apply a diff: drop entries whose name is in `removed` (order preserved),
/// replace entries named in `changed` in place, then append `added`. Daemon
/// and clients both use this so `order` indices agree on both sides. A
/// `changed` entry with no match is ignored.
pub fn apply_diff(
    entries: &mut Vec<Entry>,
    removed: &[Vec<u8>],
    added: &[Entry],
    changed: &[Entry],
) {
    if !removed.is_empty() {
        let gone: HashSet<&[u8]> = removed.iter().map(Vec::as_slice).collect();
        entries.retain(|e| !gone.contains(e.name.as_slice()));
    }
    if !changed.is_empty() {
        let new: HashMap<&[u8], &Entry> = changed.iter().map(|e| (e.name.as_slice(), e)).collect();
        for e in entries.iter_mut() {
            if let Some(&n) = new.get(e.name.as_slice()) {
                e.clone_from(n);
            }
        }
    }
    entries.extend_from_slice(added);
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] postcard::Error),
    #[error("frame of {0} bytes exceeds MAX_FRAME")]
    TooLarge(u32),
    #[error("protocol version {theirs:?} incompatible with {ours:?}")]
    Version { theirs: Version, ours: Version },
}

/// Encode `body` as one complete frame: length prefix + postcard(Envelope).
pub fn encode<T: Serialize>(body: &T) -> Result<Vec<u8>, FrameError> {
    let env = Envelope {
        version: VERSION,
        body,
    };
    let mut buf = postcard::to_extend(&env, vec![0u8; 4])?;
    let len = buf.len() - 4;
    if len > MAX_FRAME as usize {
        return Err(FrameError::TooLarge(u32::try_from(len).unwrap_or(u32::MAX)));
    }
    buf[..4].copy_from_slice(&(len as u32).to_le_bytes());
    Ok(buf)
}

/// Decode one complete frame (length prefix included). Rejects a different
/// major version before touching the body.
pub fn decode<T: DeserializeOwned>(frame: &[u8]) -> Result<T, FrameError> {
    let Some((hdr, payload)) = frame.split_first_chunk::<4>() else {
        return Err(postcard::Error::DeserializeUnexpectedEnd.into());
    };
    let len = u32::from_le_bytes(*hdr);
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    if payload.len() != len as usize {
        return Err(postcard::Error::DeserializeUnexpectedEnd.into());
    }
    decode_payload(payload)
}

/// Whether a peer speaking `theirs` can be decoded by `ours` (FOG
/// §Architecture/IPC). Postcard is not self-describing, so while the major
/// version is 0 any minor change may alter the layout and is refused; from
/// 1.0 on only the major must match.
pub fn compatible(theirs: Version, ours: Version) -> bool {
    theirs.major == ours.major && (ours.major != 0 || theirs.minor == ours.minor)
}

fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    // Envelope fields are serialized in order, so the version is a prefix.
    let (theirs, rest): (Version, _) = postcard::take_from_bytes(payload)?;
    if !compatible(theirs, VERSION) {
        return Err(FrameError::Version {
            theirs,
            ours: VERSION,
        });
    }
    Ok(postcard::from_bytes(rest)?)
}

/// Write one frame.
pub async fn write_frame<W, T>(w: &mut W, body: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let frame = encode(body)?;
    w.write_all(&frame).await?;
    Ok(())
}

/// Read one frame. `Ok(None)` on clean EOF between frames; EOF mid-frame is
/// an `Io` error. A length over [`MAX_FRAME`] is refused before allocating.
pub async fn read_frame<R, T>(r: &mut R) -> Result<Option<T>, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut hdr = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut hdr[got..]).await?;
        if n == 0 {
            if got == 0 {
                return Ok(None);
            }
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        got += n;
    }
    let len = u32::from_le_bytes(hdr);
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    decode_payload(&payload).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &[u8], kind: Kind) -> Entry {
        Entry::new(name.to_vec(), kind)
    }

    #[tokio::test]
    async fn round_trip_over_duplex() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let bad = vec![b'x', 0xff, 0xfe, b'y'];
        let req = Request::ListDir {
            path: b"/tmp/\xffdir".to_vec(),
        };
        let snap = Reply::DirSnapshot {
            path: b"/tmp".to_vec(),
            dir: 7,
            generation: 3,
            entries: vec![e(&bad, Kind::File), e(b"sub", Kind::Dir)],
            order: vec![1, 0],
            complete: false,
        };
        let stat = Reply::Stat(StatReply {
            path: bad.clone(),
            kind: Kind::Symlink,
            size: 42,
            mtime_ns: -1_000_000_007,
            inode: u64::MAX,
        });
        write_frame(&mut a, &req).await.unwrap();
        write_frame(&mut a, &snap).await.unwrap();
        write_frame(&mut a, &stat).await.unwrap();
        drop(a);
        assert_eq!(read_frame::<_, Request>(&mut b).await.unwrap(), Some(req));
        let got: Reply = read_frame(&mut b).await.unwrap().unwrap();
        assert_eq!(got, snap);
        if let Reply::DirSnapshot { entries, .. } = &got {
            assert_eq!(entries[0].name, bad);
            assert_eq!(entries[0].display(), "x\u{fffd}\u{fffd}y");
        }
        assert_eq!(read_frame::<_, Reply>(&mut b).await.unwrap(), Some(stat));
        assert!(read_frame::<_, Reply>(&mut b).await.unwrap().is_none());
    }

    #[test]
    fn encode_decode_sync() {
        let r = Reply::Error {
            path: b"/nope".to_vec(),
            errno: 2,
        };
        assert_eq!(decode::<Reply>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[tokio::test]
    async fn unknown_major_rejected() {
        let env = Envelope {
            version: Version {
                major: VERSION.major + 1,
                minor: 0,
            },
            body: Request::Stat {
                path: b"/".to_vec(),
            },
        };
        let payload = postcard::to_allocvec(&env).unwrap();
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&payload);
        assert!(matches!(
            decode::<Request>(&frame),
            Err(FrameError::Version { theirs, ours }) if theirs.major == 1 && ours == VERSION
        ));
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&frame).await.unwrap();
        assert!(matches!(
            read_frame::<_, Request>(&mut b).await,
            Err(FrameError::Version { .. })
        ));
    }

    #[test]
    fn zero_major_minor_mismatch_rejected() {
        let v = |major, minor| Version { major, minor };
        assert!(!compatible(v(0, 1), v(0, 2)));
        assert!(!compatible(v(0, 2), v(0, 1)));
        assert!(compatible(v(0, 2), v(0, 2)));
        assert!(compatible(v(1, 3), v(1, 7)));
        assert!(compatible(v(1, 7), v(1, 3)));
        assert!(!compatible(v(2, 0), v(1, 0)));

        // Over the wire: a 0.1 frame is refused by this 0.2 build.
        let env = Envelope {
            version: v(0, 1),
            body: Request::Undo,
        };
        let payload = postcard::to_allocvec(&env).unwrap();
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&payload);
        assert!(matches!(
            decode::<Request>(&frame),
            Err(FrameError::Version { theirs, .. }) if theirs == v(0, 1)
        ));
    }

    #[tokio::test]
    async fn oversized_length_refused() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(MAX_FRAME + 1).to_le_bytes()).await.unwrap();
        assert!(matches!(
            read_frame::<_, Request>(&mut b).await,
            Err(FrameError::TooLarge(n)) if n == MAX_FRAME + 1
        ));
        let mut frame = u32::MAX.to_le_bytes().to_vec();
        frame.push(0);
        assert!(matches!(
            decode::<Request>(&frame),
            Err(FrameError::TooLarge(u32::MAX))
        ));
    }

    #[tokio::test]
    async fn eof_mid_header_is_error() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&[1, 0]).await.unwrap();
        drop(a);
        assert!(matches!(
            read_frame::<_, Request>(&mut b).await,
            Err(FrameError::Io(_))
        ));
    }

    #[test]
    fn apply_diff_retains_order_then_appends() {
        let mut v = vec![
            e(b"a", Kind::File),
            e(b"b", Kind::Dir),
            e(b"c", Kind::File),
            e(b"d", Kind::Other),
        ];
        apply_diff(
            &mut v,
            &[b"b".to_vec(), b"zz".to_vec()],
            &[e(b"e", Kind::Symlink)],
            &[],
        );
        let names: Vec<&[u8]> = v.iter().map(|e| e.name.as_slice()).collect();
        assert_eq!(names, [&b"a"[..], b"c", b"d", b"e"]);
        apply_diff(&mut v, &[], &[], &[]);
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn apply_diff_changed_updates_in_place() {
        let mut v = vec![e(b"a", Kind::File), e(b"b", Kind::File)];
        let b2 = Entry {
            size: Some(9),
            mtime_ns: Some(-5),
            mode: Some(0o100644),
            ..e(b"b", Kind::File)
        };
        apply_diff(
            &mut v,
            &[b"a".to_vec()],
            &[e(b"c", Kind::Dir)],
            &[b2.clone(), e(b"ghost", Kind::File)],
        );
        assert_eq!(v, [b2, e(b"c", Kind::Dir)]);
    }

    #[test]
    fn v02_requests_round_trip() {
        let p = |s: &[u8]| s.to_vec();
        let reqs = vec![
            Request::Subscribe { path: p(b"/\xffd") },
            Request::Unsubscribe { dir: u64::MAX },
            Request::Job(JobSpec::Copy {
                srcs: vec![p(b"/a"), p(b"/\xfeb")],
                dest: p(b"/d"),
                on_conflict: ConflictPolicy::Ask,
            }),
            Request::Job(JobSpec::Move {
                srcs: vec![p(b"/a")],
                dest: p(b"/d"),
                on_conflict: ConflictPolicy::Skip,
            }),
            Request::Job(JobSpec::Rename {
                path: p(b"/a"),
                new_name: p(b"b\xff"),
                on_conflict: ConflictPolicy::Rename,
            }),
            Request::Job(JobSpec::Trash {
                paths: vec![p(b"/a")],
                on_conflict: ConflictPolicy::Replace,
            }),
            Request::Job(JobSpec::Delete {
                paths: vec![],
                on_conflict: ConflictPolicy::default(),
            }),
            Request::Job(JobSpec::Mkdir {
                path: p(b"/n"),
                on_conflict: ConflictPolicy::Fail,
            }),
            Request::Job(JobSpec::CreateFile {
                path: p(b"/f"),
                on_conflict: ConflictPolicy::Fail,
            }),
            Request::Job(JobSpec::Restore {
                trash_ids: vec![p(b"t1"), p(b"t2")],
                on_conflict: ConflictPolicy::Ask,
            }),
            Request::JobControl {
                id: 1,
                action: JobAction::Pause,
            },
            Request::JobControl {
                id: 2,
                action: JobAction::Resume,
            },
            Request::JobControl {
                id: 3,
                action: JobAction::Cancel,
            },
            Request::JobControl {
                id: 4,
                action: JobAction::Resolve {
                    choice: Resolution::KeepBoth,
                    apply_all: true,
                },
            },
            Request::JobControl {
                id: 5,
                action: JobAction::Resolve {
                    choice: Resolution::Merge,
                    apply_all: false,
                },
            },
            Request::Undo,
            Request::Open {
                path: p(b"/x.txt"),
                app: Some("micro.desktop".into()),
            },
            Request::Open {
                path: p(b"/x.txt"),
                app: None,
            },
            Request::ListTrash,
            Request::Places,
        ];
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::Fail);
        for r in reqs {
            assert_eq!(decode::<Request>(&encode(&r).unwrap()).unwrap(), r);
        }
    }

    #[tokio::test]
    async fn v02_replies_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let full = Entry {
            size: Some(u64::MAX),
            mtime_ns: Some(i128::MIN),
            mode: Some(0o40755),
            ..e(b"\xffx", Kind::Dir)
        };
        let replies = vec![
            Reply::DirDiff {
                dir: 1,
                generation: 2,
                removed: vec![b"gone".to_vec()],
                added: vec![e(b"new", Kind::File)],
                changed: vec![full],
                order: vec![1, 0],
                complete: true,
            },
            Reply::JobAccepted { id: 7 },
            Reply::JobProgress {
                id: 7,
                bytes_done: 8 << 20,
                bytes_total: u64::MAX,
                files_done: 1,
                files_total: 3,
                current: b"/src/\xff".to_vec(),
            },
            Reply::JobState {
                id: 7,
                state: JobStatus::Conflict {
                    src: b"/a/f".to_vec(),
                    dest: b"/b/f".to_vec(),
                },
            },
            Reply::JobState {
                id: 7,
                state: JobStatus::Failed {
                    errno: 28,
                    msg: "no space".into(),
                },
            },
            Reply::JobState {
                id: 8,
                state: JobStatus::Cancelled,
            },
            Reply::UndoResult {
                ok: false,
                reason: Some("target changed since the operation".into()),
            },
            Reply::UndoResult {
                ok: true,
                reason: None,
            },
            Reply::TrashList(vec![TrashItem {
                id: b"f.txt".to_vec(),
                original_path: b"/home/u/f.txt".to_vec(),
                deleted_s: Some(1_790_000_000),
                kind: Kind::File,
                size: None,
            }]),
            Reply::PlacesList(vec![
                Place {
                    kind: PlaceKind::Home,
                    label: "Home".into(),
                    path: b"/home/u".to_vec(),
                },
                Place {
                    kind: PlaceKind::Bookmark,
                    label: "src".into(),
                    path: b"/home/u/\xffsrc".to_vec(),
                },
            ]),
            Reply::ConfigError {
                line: 3,
                col: 5,
                msg: "unknown key `shwo-hidden`".into(),
            },
            Reply::Opened {
                path: b"/x.txt".to_vec(),
            },
        ];
        for r in &replies {
            write_frame(&mut a, r).await.unwrap();
        }
        drop(a);
        for r in replies {
            assert_eq!(read_frame::<_, Reply>(&mut b).await.unwrap(), Some(r));
        }
        assert!(read_frame::<_, Reply>(&mut b).await.unwrap().is_none());
    }
}
