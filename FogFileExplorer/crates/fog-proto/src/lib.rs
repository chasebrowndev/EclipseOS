// SPDX-License-Identifier: AGPL-3.0-only

//! Fog's IPC wire types and framing (FOG §Architecture/IPC). No filesystem I/O.
//!
//! A frame is a little-endian `u32` payload length followed by the postcard
//! encoding of an [`Envelope`]. Every frame carries [`VERSION`]; a peer with a
//! different major version is refused. Paths and names are raw bytes, never
//! assumed to be UTF-8.

use std::borrow::Cow;
use std::collections::HashSet;

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
pub const VERSION: Version = Version { major: 0, minor: 1 };

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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Entry {
    pub name: Vec<u8>,
    pub kind: Kind,
}

impl Entry {
    /// The name for display: lossy UTF-8.
    pub fn display(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }
}

/// Client to `fogd`. Paths are absolute, raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    ListDir { path: Vec<u8> },
    Stat { path: Vec<u8> },
}

/// `fogd` to client.
///
/// `order` is the display order as indices into the entry list. After a
/// `DirDiff`, indices refer to the list produced by [`apply_diff`].
/// `complete == false` means more entries follow as `DirDiff`s.
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
        order: Vec<u32>,
        complete: bool,
    },
    Stat(StatReply),
    Error {
        path: Vec<u8>,
        errno: i32,
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
/// then append `added`. Daemon and clients both use this so `order` indices
/// agree on both sides.
pub fn apply_diff(entries: &mut Vec<Entry>, removed: &[Vec<u8>], added: &[Entry]) {
    if !removed.is_empty() {
        let gone: HashSet<&[u8]> = removed.iter().map(Vec::as_slice).collect();
        entries.retain(|e| !gone.contains(e.name.as_slice()));
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

fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    // Envelope fields are serialized in order, so the version is a prefix.
    let (theirs, rest): (Version, _) = postcard::take_from_bytes(payload)?;
    if theirs.major != VERSION.major {
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
        Entry {
            name: name.to_vec(),
            kind,
        }
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
        );
        let names: Vec<&[u8]> = v.iter().map(|e| e.name.as_slice()).collect();
        assert_eq!(names, [&b"a"[..], b"c", b"d", b"e"]);
        apply_diff(&mut v, &[], &[]);
        assert_eq!(v.len(), 4);
    }
}
