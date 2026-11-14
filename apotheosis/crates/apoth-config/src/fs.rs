// SPDX-License-Identifier: AGPL-3.0-only
//! File access behind a trait so layers, includes and edits are testable without a disk,
//! plus the atomic write (temp + fsync + rename) and the content hash used to suppress the
//! watcher's reaction to our own writes (CONFIG-16; the watcher itself is M15).

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// Largest config file the loader will read (T-10 limits: bounded input).
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// What the loader and editor need from a filesystem.
pub trait Fs {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn exists(&self, path: &Path) -> bool;
    /// Entries of a directory (full paths), unsorted. A missing directory is `Ok(vec![])`.
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>>;
    /// Write the whole file atomically.
    fn write_atomic(&self, path: &Path, data: &str) -> io::Result<()>;
    /// A path with symlinks resolved where the filesystem can, otherwise lexically cleaned.
    fn canonical(&self, path: &Path) -> PathBuf {
        normalize(path)
    }
}

/// Lexically remove `.` and resolve `..` (no filesystem access).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The real filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

impl Fs for RealFs {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        use std::io::Read;
        // Regular files only (a FIFO would block, /dev/zero never ends), size-capped.
        let meta = std::fs::metadata(path)?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
        }
        let mut buf = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut buf)?;
        if buf.len() as u64 > MAX_FILE_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
        }
        String::from_utf8(buf)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not valid UTF-8"))
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        match std::fs::read_dir(dir) {
            // A bad entry (EACCES, removed mid-read) is skipped, not fatal to the listing.
            Ok(rd) => Ok(rd.filter_map(|e| e.ok().map(|e| e.path())).collect()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn write_atomic(&self, path: &Path, data: &str) -> io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config".into());
        let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
        let result: io::Result<()> = (|| {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(data.as_bytes())?;
            f.sync_all()?;
            drop(f);
            // Keep the mode of the file being replaced.
            if let Ok(meta) = std::fs::metadata(path) {
                let _ = std::fs::set_permissions(&tmp, meta.permissions());
            }
            std::fs::rename(&tmp, path)?;
            // Make the rename durable.
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    fn canonical(&self, path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path))
    }
}

/// An in-memory filesystem for tests and for ec-settings previews.
#[derive(Debug, Default)]
pub struct MemFs {
    files: Mutex<BTreeMap<PathBuf, String>>,
}

impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace a file; returns self for chaining.
    pub fn with(self, path: impl AsRef<Path>, text: &str) -> Self {
        self.set(path, text);
        self
    }

    pub fn set(&self, path: impl AsRef<Path>, text: &str) {
        if let Ok(mut g) = self.files.lock() {
            g.insert(normalize(path.as_ref()), text.to_string());
        }
    }

    pub fn get(&self, path: impl AsRef<Path>) -> Option<String> {
        self.files
            .lock()
            .ok()
            .and_then(|g| g.get(&normalize(path.as_ref())).cloned())
    }
}

impl Fs for MemFs {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.get(path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
    }

    fn exists(&self, path: &Path) -> bool {
        self.get(path).is_some()
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let dir = normalize(dir);
        let g = self
            .files
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        Ok(g.keys()
            .filter(|p| p.parent() == Some(dir.as_path()))
            .cloned()
            .collect())
    }

    fn write_atomic(&self, path: &Path, data: &str) -> io::Result<()> {
        self.set(path, data);
        Ok(())
    }
}

/// Match a file name against a pattern with `*` (any run) and `?` (one char).
pub fn glob_match(pat: &str, name: &str) -> bool {
    // Iterative two-pointer match with single-star backtracking: O(len(p) * len(n)).
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Does the pattern contain glob metacharacters?
pub fn has_glob(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

/// Expand an include pattern. Globs apply to the final path component only; matches are
/// sorted by name so the order is deterministic. A plain path is returned as is (existing or
/// not), so the caller can report a missing file.
pub fn expand_glob(fs: &dyn Fs, pattern: &Path) -> Vec<PathBuf> {
    let name = pattern
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !has_glob(&name) {
        return vec![pattern.to_path_buf()];
    }
    let dir = pattern.parent().unwrap_or_else(|| Path::new("."));
    let mut out: Vec<PathBuf> = fs
        .list_dir(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            p.file_name()
                .map(|n| glob_match(&name, &n.to_string_lossy()))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

/// FNV-1a 64-bit hash of file content.
pub fn content_hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Remembers the hash of what we last wrote to each file so the watcher can ignore the event
/// our own write causes (CONFIG-16).
#[derive(Debug, Default)]
pub struct SelfWrites {
    hashes: BTreeMap<PathBuf, u64>,
}

impl SelfWrites {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, path: &Path, text: &str) {
        self.hashes.insert(normalize(path), content_hash(text));
    }

    /// True if `text` is exactly what we last wrote to `path`.
    pub fn is_self_write(&self, path: &Path, text: &str) -> bool {
        self.hashes.get(&normalize(path)) == Some(&content_hash(text))
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    #[test]
    fn glob_match_is_not_exponential() {
        let name = "a".repeat(40);
        assert!(!glob_match("*a*a*a*a*a*a*a*a*b", &name));
        assert!(glob_match("*a*a*a*a*a*a*a*a*", &name));
        assert!(glob_match("a?c*", "abcdef"));
        assert!(!glob_match("a?c", "abcd"));
    }

    #[test]
    fn real_fs_refuses_non_regular_and_oversized() {
        let fs = RealFs;
        assert!(fs.read_to_string(Path::new("/dev/zero")).is_err());
        let dir = std::env::temp_dir().join(format!("apoth-cfg-fs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.kdl");
        std::fs::write(&big, vec![b'a'; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(fs.read_to_string(&big).is_err());
        let ok = dir.join("ok.kdl");
        std::fs::write(&ok, "x").unwrap();
        assert_eq!(fs.read_to_string(&ok).unwrap(), "x");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
