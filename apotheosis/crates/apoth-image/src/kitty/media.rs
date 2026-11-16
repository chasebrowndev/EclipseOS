// SPDX-License-Identifier: AGPL-3.0-only
//! File, temp-file and shm transmission (T-15 §1 file opening rules, T-18 §3, §10, §11).
//!
//! The policy decision (`ask` resolved through the UI, remote refusal,
//! `graphics.file-transmission = off`) is an *input*: [`Decision`]. Denied gives no effect
//! and no reply. Every other failure -- not found, permission, wrong type, oversize,
//! traversal, symlink, FIFO -- yields the one identical [`MediaResult::Failed`], which the
//! caller answers with `ErrCode::EnoentFile`.

use super::Medium;
use crate::Limits;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

/// Outcome of the policy class `graphics-file` for this request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Denied by policy, remote pane, or `graphics.file-transmission = off`.
    Deny,
}

/// Where temp files may live, and the remaining pane quota (checked before any read).
#[derive(Debug, Clone)]
pub struct MediaPolicy {
    pub decision: Decision,
    /// Known temp dirs for `t=t` (the file name must also contain `tty-graphics-protocol`).
    pub temp_dirs: Vec<PathBuf>,
    /// Bytes still available under the pane quota; reads larger than this fail.
    pub quota_remaining: u64,
}

impl MediaPolicy {
    pub fn local_allow(quota_remaining: u64) -> Self {
        Self {
            decision: Decision::Allow,
            temp_dirs: vec!["/tmp".into(), "/var/tmp".into()],
            quota_remaining,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MediaResult {
    /// Bytes read (at most `S`, starting at `O`).
    Data(Vec<u8>),
    /// Policy denied: no observable effect, no reply.
    Denied,
    /// Any check failed: reply `ENOENT`, identical for every cause.
    Failed,
}

// Linux open(2) flags. O_CLOEXEC is always set by std. aarch64/arm differ from x86.
#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
const O_NOFOLLOW: i32 = 0o100000;
#[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
const O_NOFOLLOW: i32 = 0o400000;
const O_NONBLOCK: i32 = 0o4000;

fn refused_prefix(p: &Path, allow_dev_shm: bool) -> bool {
    if allow_dev_shm && p.starts_with("/dev/shm") {
        return false;
    }
    ["/proc", "/sys", "/dev"].iter().any(|r| p.starts_with(r))
}

/// Load the bytes a file-type transmission names. `payload` is the base64-decoded path
/// (or shm name for [`Medium::Shm`]); `size`/`offset` are `S=`/`O=`.
pub fn load_media(
    medium: Medium,
    payload: &[u8],
    size: u32,
    offset: u32,
    policy: &MediaPolicy,
    limits: &Limits,
) -> MediaResult {
    if policy.decision == Decision::Deny || medium == Medium::Direct {
        return if medium == Medium::Direct {
            MediaResult::Failed
        } else {
            MediaResult::Denied
        };
    }
    load_inner(medium, payload, size, offset, policy, limits)
        .map_or(MediaResult::Failed, MediaResult::Data)
}

#[cfg(unix)]
fn load_inner(
    medium: Medium,
    payload: &[u8],
    size: u32,
    offset: u32,
    policy: &MediaPolicy,
    limits: &Limits,
) -> Option<Vec<u8>> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;

    if payload.is_empty() || payload.len() > 4096 || payload.contains(&0) {
        return None;
    }
    let raw = Path::new(OsStr::from_bytes(payload));
    // Shm: a bare name, opened as /dev/shm/<name> (the Linux shm_open mapping).
    let path: PathBuf = if medium == Medium::Shm {
        let name = payload.strip_prefix(b"/").unwrap_or(payload);
        if name.is_empty() || name.contains(&b'/') || name == b"." || name == b".." {
            return None;
        }
        Path::new("/dev/shm").join(OsStr::from_bytes(name))
    } else {
        raw.to_path_buf()
    };
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let shm = medium == Medium::Shm;
    if refused_prefix(&path, shm) {
        return None;
    }
    // Resolve the directory (intermediate symlinks), then re-check the resolved location.
    let parent = std::fs::canonicalize(path.parent()?).ok()?;
    let leaf = path.file_name()?;
    let resolved = parent.join(leaf);
    if refused_prefix(&resolved, shm) {
        return None;
    }
    if medium == Medium::TempFile {
        let under_temp = policy.temp_dirs.iter().any(|d| {
            resolved.starts_with(d)
                || std::fs::canonicalize(d).is_ok_and(|c| resolved.starts_with(c))
        });
        let name_ok = leaf
            .as_bytes()
            .windows(21)
            .any(|w| w == b"tty-graphics-protocol")
            || parent
                .as_os_str()
                .as_bytes()
                .windows(21)
                .any(|w| w == b"tty-graphics-protocol");
        if !under_temp || !name_ok {
            return None;
        }
    }
    let mut f: File = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(&resolved)
        .ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let len = meta.len();
    let offset = u64::from(offset);
    if offset > len {
        return None;
    }
    let avail = len - offset;
    let want = if size == 0 {
        avail
    } else {
        u64::from(size).min(avail)
    };
    // Cap checks happen before any read.
    if want > limits.image_cap() || want > policy.quota_remaining {
        return None;
    }
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::with_capacity(usize::try_from(want).ok()?);
    f.take(want).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 != want {
        return None;
    }
    // Temp files and shm objects are consumed by the read.
    if medium != Medium::File {
        let _ = std::fs::remove_file(&resolved);
    }
    Some(buf)
}

#[cfg(not(unix))]
fn load_inner(_: Medium, _: &[u8], _: u32, _: u32, _: &MediaPolicy, _: &Limits) -> Option<Vec<u8>> {
    None
}
