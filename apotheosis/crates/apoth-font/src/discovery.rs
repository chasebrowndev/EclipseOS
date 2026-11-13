// SPDX-License-Identifier: AGPL-3.0-only
//! Font discovery (T-06 §1, §4): our own directory scan over the directories named by
//! `fontconfig-parser` (and the XDG defaults), a per-face index of name / OS/2 / fvar / cmap
//! facts read once through `read-fonts`, and an on-disk cache of that index keyed by
//! path + mtime + size. Fallback lookup is a binary search over sorted code-point ranges and
//! never opens a font.

use crate::face::{ColorSources, color_sources, family_name};
use crate::ranges::CodepointRanges;
use skrifa::{
    FontRef, MetadataProvider,
    raw::{FileRef, types::Tag},
};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAGIC: &[u8; 4] = b"APFI";
/// Bump when the on-disk layout changes; older caches are discarded.
pub const INDEX_VERSION: u32 = 1;
const MAX_FILES: u32 = 1 << 20;
const MAX_FACES_PER_FILE: u32 = 1 << 12;
const MAX_STR: usize = 4096;
const MAX_DEPTH: usize = 16;

/// What the index knows about one face.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexedFace {
    /// Index inside a collection (0 for plain fonts).
    pub ttc_index: u32,
    /// Typographic family (name id 16, else 1).
    pub family: String,
    /// OS/2 weight class.
    pub weight: u16,
    /// Italic or oblique.
    pub italic: bool,
    /// `post.isFixedPitch`.
    pub monospace: bool,
    /// Range of an `fvar` `wght` axis, if any.
    pub wght_axis: Option<(f32, f32)>,
    /// Colour sources.
    pub color: ColorSources,
    /// Sorted code-point coverage.
    pub ranges: CodepointRanges,
}

/// One font file and its faces. `faces` is empty when the file did not parse (kept so the
/// failure is not retried until the file changes).
#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    /// Absolute path (or `embedded:<name>`).
    pub path: PathBuf,
    /// Modification time, nanoseconds since the epoch.
    pub mtime_ns: u64,
    /// Size in bytes.
    pub size: u64,
    /// Faces found.
    pub faces: Vec<IndexedFace>,
}

/// The index of every discovered face.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontIndex {
    /// Files, sorted by path.
    pub files: Vec<FileEntry>,
}

/// Why a cache could not be read. Carries no content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexError {
    /// Wrong magic or version.
    Header,
    /// Truncated or inconsistent data.
    Corrupt,
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header => f.write_str("font index header mismatch"),
            Self::Corrupt => f.write_str("font index is corrupt"),
        }
    }
}

impl std::error::Error for IndexError {}

/// Counters from a build.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildStats {
    /// Files reused from the previous index.
    pub reused: usize,
    /// Files parsed afresh.
    pub parsed: usize,
}

// ---------------------------------------------------------------- directories

/// Directories to scan: those fontconfig names (`<dir>`, `<include>`d files), the XDG
/// defaults, then `extra`. Only existing directories, de-duplicated, in that order.
pub fn font_dirs(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut fc = fontconfig_parser::FontConfig::default();
    for conf in ["/etc/fonts/fonts.conf", "/usr/etc/fonts/fonts.conf"] {
        if Path::new(conf).exists() && fc.merge_config(conf).is_ok() {
            break;
        }
    }
    out.extend(fc.dirs.iter().map(|d| d.path.clone()));
    out.extend(xdg_font_dirs());
    out.extend(extra.iter().cloned());
    let mut seen = HashSet::new();
    out.retain(|p| p.is_dir() && seen.insert(fs::canonicalize(p).unwrap_or_else(|_| p.clone())));
    out
}

fn xdg_font_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut v = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
        Some(d) => v.push(PathBuf::from(d).join("fonts")),
        None => v.extend(home.as_ref().map(|h| h.join(".local/share/fonts"))),
    }
    v.extend(home.as_ref().map(|h| h.join(".fonts")));
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    v.extend(
        dirs.split(':')
            .filter(|s| !s.is_empty())
            .map(|d| PathBuf::from(d).join("fonts")),
    );
    v
}

fn is_font_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "ttf" | "otf" | "ttc" | "otc"
        )
    })
}

/// Recursively lists font files under `dirs` with their stamps, sorted by path.
pub fn scan_files(dirs: &[PathBuf]) -> Vec<(PathBuf, u64, u64)> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    for d in dirs {
        walk(d, 0, &mut visited, &mut out);
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

fn walk(
    dir: &Path,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<(PathBuf, u64, u64)>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(canon) = fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(canon) {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(&path, depth + 1, visited, out);
        } else if meta.is_file() && is_font_file(&path) {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as u64);
            out.push((path, mtime, meta.len()));
        }
    }
}

// ------------------------------------------------------------------- indexing

/// Reads the index facts of every face in `data`. Never panics on arbitrary bytes.
pub fn index_font_bytes(data: &[u8]) -> Vec<IndexedFace> {
    let Ok(file) = FileRef::new(data) else {
        return Vec::new();
    };
    let mut faces = Vec::new();
    match file {
        FileRef::Font(f) => faces.extend(index_face(&f, 0)),
        FileRef::Collection(c) => {
            for i in 0..c.len().min(MAX_FACES_PER_FILE) {
                if let Ok(f) = c.get(i) {
                    faces.extend(index_face(&f, i));
                }
            }
        }
    }
    faces
}

fn index_face(font: &FontRef<'_>, ttc_index: u32) -> Option<IndexedFace> {
    let cps = font.charmap().mappings().map(|(c, _)| c);
    let ranges = CodepointRanges::from_codepoints(cps.take(0x11_0000));
    if ranges.is_empty() {
        return None;
    }
    let attrs = font.attributes();
    let wght_axis = font
        .axes()
        .get_by_tag(Tag::new(b"wght"))
        .map(|a| (a.min_value(), a.max_value()));
    let monospace = font
        .metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        )
        .is_monospace;
    Some(IndexedFace {
        ttc_index,
        family: family_name(font).unwrap_or_default(),
        weight: attrs.weight.value().round().clamp(1.0, 1000.0) as u16,
        italic: !matches!(attrs.style, skrifa::attribute::Style::Normal),
        monospace,
        wght_axis,
        color: color_sources(font),
        ranges,
    })
}

impl FontIndex {
    /// Scans `dirs`, reusing entries of `previous` whose path, mtime and size are unchanged.
    pub fn build(dirs: &[PathBuf], previous: Option<&FontIndex>) -> (FontIndex, BuildStats) {
        let prev: HashMap<&Path, &FileEntry> = previous
            .map(|p| p.files.iter().map(|f| (f.path.as_path(), f)).collect())
            .unwrap_or_default();
        let mut stats = BuildStats::default();
        let mut files = Vec::new();
        for (path, mtime_ns, size) in scan_files(dirs) {
            if let Some(old) = prev.get(path.as_path())
                && old.mtime_ns == mtime_ns
                && old.size == size
            {
                files.push((*old).clone());
                stats.reused += 1;
                continue;
            }
            let faces = fs::read(&path)
                .map(|d| index_font_bytes(&d))
                .unwrap_or_default();
            stats.parsed += 1;
            files.push(FileEntry {
                path,
                mtime_ns,
                size,
                faces,
            });
        }
        (FontIndex { files }, stats)
    }

    /// Index of in-memory fonts (the embedded fonts). Paths are `embedded:<name>`.
    pub fn from_memory<'a>(fonts: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> FontIndex {
        let files = fonts
            .into_iter()
            .map(|(name, data)| FileEntry {
                path: PathBuf::from(format!("embedded:{name}")),
                mtime_ns: 0,
                size: data.len() as u64,
                faces: index_font_bytes(data),
            })
            .collect();
        FontIndex { files }
    }

    /// Total number of faces.
    pub fn face_count(&self) -> usize {
        self.files.iter().map(|f| f.faces.len()).sum()
    }

    /// Faces whose ranges cover `cp`, as `(file, face)` positions. Binary search per face; no
    /// font is opened.
    pub fn faces_covering(&self, cp: u32) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.files.iter().enumerate().flat_map(move |(fi, f)| {
            f.faces
                .iter()
                .enumerate()
                .filter(move |(_, face)| face.ranges.contains(cp))
                .map(move |(i, _)| (fi, i))
        })
    }

    // ------------------------------------------------------------- persistence

    /// Serialises the index.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(MAGIC);
        w.extend_from_slice(&INDEX_VERSION.to_le_bytes());
        put_u32(&mut w, self.files.len() as u32);
        for f in &self.files {
            put_bytes(&mut w, f.path.as_os_str().as_bytes());
            w.extend_from_slice(&f.mtime_ns.to_le_bytes());
            w.extend_from_slice(&f.size.to_le_bytes());
            put_u32(&mut w, f.faces.len() as u32);
            for face in &f.faces {
                put_u32(&mut w, face.ttc_index);
                put_bytes(&mut w, face.family.as_bytes());
                w.extend_from_slice(&face.weight.to_le_bytes());
                let c = face.color;
                let flags = u8::from(face.italic)
                    | u8::from(face.monospace) << 1
                    | u8::from(face.wght_axis.is_some()) << 2
                    | u8::from(c.colr) << 3
                    | u8::from(c.cbdt) << 4
                    | u8::from(c.sbix) << 5
                    | u8::from(c.svg) << 6;
                w.push(flags);
                let (lo, hi) = face.wght_axis.unwrap_or((0.0, 0.0));
                w.extend_from_slice(&lo.to_le_bytes());
                w.extend_from_slice(&hi.to_le_bytes());
                put_varint(&mut w, face.ranges.ranges().len() as u64);
                let mut prev_end = 0u32;
                for &(a, b) in face.ranges.ranges() {
                    put_varint(&mut w, u64::from(a - prev_end));
                    put_varint(&mut w, u64::from(b - a));
                    prev_end = b;
                }
            }
        }
        w
    }

    /// Parses a serialised index. Total: any byte string yields `Ok` or `Err`, never a panic,
    /// and allocation is bounded by the input length.
    pub fn from_bytes(bytes: &[u8]) -> Result<FontIndex, IndexError> {
        let mut r = Reader { b: bytes, pos: 0 };
        if r.take(4)? != MAGIC {
            return Err(IndexError::Header);
        }
        if r.u32()? != INDEX_VERSION {
            return Err(IndexError::Header);
        }
        let nfiles = r.u32()?;
        if nfiles > MAX_FILES || (nfiles as usize) > bytes.len() {
            return Err(IndexError::Corrupt);
        }
        let mut files = Vec::with_capacity(nfiles as usize);
        for _ in 0..nfiles {
            let path = PathBuf::from(std::ffi::OsString::from_vec(r.bytes()?.to_vec()));
            let mtime_ns = r.u64()?;
            let size = r.u64()?;
            let nfaces = r.u32()?;
            if nfaces > MAX_FACES_PER_FILE || (nfaces as usize) > bytes.len() {
                return Err(IndexError::Corrupt);
            }
            let mut faces = Vec::with_capacity(nfaces as usize);
            for _ in 0..nfaces {
                let ttc_index = r.u32()?;
                let family =
                    String::from_utf8(r.bytes()?.to_vec()).map_err(|_| IndexError::Corrupt)?;
                let weight = r.u16()?;
                let flags = r.u8()?;
                let lo =
                    f32::from_le_bytes(r.take(4)?.try_into().map_err(|_| IndexError::Corrupt)?);
                let hi =
                    f32::from_le_bytes(r.take(4)?.try_into().map_err(|_| IndexError::Corrupt)?);
                let n = r.varint()?;
                if n > 0x11_0000 || n as usize > bytes.len() {
                    return Err(IndexError::Corrupt);
                }
                let mut v = Vec::with_capacity(n as usize);
                let mut prev_end = 0u64;
                for i in 0..n {
                    let gap = r.varint()?;
                    let len = r.varint()?;
                    let a = prev_end.checked_add(gap).ok_or(IndexError::Corrupt)?;
                    let b = a.checked_add(len).ok_or(IndexError::Corrupt)?;
                    // Adjacent ranges are merged on write, so a zero gap is only valid first.
                    if b > 0x10_FFFF || (i > 0 && gap == 0) {
                        return Err(IndexError::Corrupt);
                    }
                    v.push((a as u32, b as u32));
                    prev_end = b;
                }
                let ranges = CodepointRanges::from_ranges(v);
                faces.push(IndexedFace {
                    ttc_index,
                    family,
                    weight,
                    italic: flags & 1 != 0,
                    monospace: flags & 2 != 0,
                    wght_axis: (flags & 4 != 0 && lo.is_finite() && hi.is_finite())
                        .then_some((lo, hi)),
                    color: ColorSources {
                        colr: flags & 8 != 0,
                        cbdt: flags & 16 != 0,
                        sbix: flags & 32 != 0,
                        svg: flags & 64 != 0,
                    },
                    ranges,
                });
            }
            files.push(FileEntry {
                path,
                mtime_ns,
                size,
                faces,
            });
        }
        if r.pos != bytes.len() {
            return Err(IndexError::Corrupt);
        }
        Ok(FontIndex { files })
    }

    /// Loads the cache at `cache_file` (ignoring a missing or corrupt one), re-scans `dirs`
    /// reusing unchanged entries, and rewrites the cache if anything changed.
    pub fn load_or_build(dirs: &[PathBuf], cache_file: &Path) -> (FontIndex, BuildStats) {
        let old = fs::read(cache_file)
            .ok()
            .and_then(|b| FontIndex::from_bytes(&b).ok());
        let (idx, stats) = Self::build(dirs, old.as_ref());
        let changed = stats.parsed > 0
            || old
                .as_ref()
                .is_none_or(|o| o.files.len() != idx.files.len());
        if changed {
            let _ = write_atomic(cache_file, &idx.to_bytes());
        }
        (idx, stats)
    }
}

/// `$XDG_CACHE_HOME/apotheosis/font-index.bin` (`~/.cache` when unset).
pub fn default_cache_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("apotheosis").join("font-index.bin"))
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

fn put_u32(w: &mut Vec<u8>, v: u32) {
    w.extend_from_slice(&v.to_le_bytes());
}

fn put_bytes(w: &mut Vec<u8>, b: &[u8]) {
    let b = &b[..b.len().min(MAX_STR)];
    put_u32(w, b.len() as u32);
    w.extend_from_slice(b);
}

fn put_varint(w: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        w.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
    w.push(v as u8);
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], IndexError> {
        let end = self.pos.checked_add(n).ok_or(IndexError::Corrupt)?;
        let s = self.b.get(self.pos..end).ok_or(IndexError::Corrupt)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, IndexError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, IndexError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().map_err(|_| IndexError::Corrupt)?,
        ))
    }
    fn u32(&mut self) -> Result<u32, IndexError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| IndexError::Corrupt)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, IndexError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| IndexError::Corrupt)?,
        ))
    }
    fn bytes(&mut self) -> Result<&'a [u8], IndexError> {
        let n = self.u32()? as usize;
        if n > MAX_STR {
            return Err(IndexError::Corrupt);
        }
        self.take(n)
    }
    fn varint(&mut self) -> Result<u64, IndexError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(IndexError::Corrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FontIndex {
        FontIndex {
            files: vec![FileEntry {
                path: PathBuf::from("/x/A.ttf"),
                mtime_ns: 42,
                size: 7,
                faces: vec![IndexedFace {
                    ttc_index: 0,
                    family: "Test Mono".into(),
                    weight: 400,
                    italic: false,
                    monospace: true,
                    wght_axis: Some((100.0, 900.0)),
                    color: ColorSources {
                        colr: true,
                        ..Default::default()
                    },
                    ranges: CodepointRanges::from_ranges([(0x20, 0x7E), (0xE0B0, 0xE0BF)]),
                }],
            }],
        }
    }

    #[test]
    fn roundtrip() {
        let i = sample();
        assert_eq!(FontIndex::from_bytes(&i.to_bytes()).unwrap(), i);
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        let good = sample().to_bytes();
        assert_eq!(FontIndex::from_bytes(b"nope"), Err(IndexError::Header));
        assert_eq!(FontIndex::from_bytes(&[]), Err(IndexError::Corrupt));
        for n in 0..good.len() {
            assert!(FontIndex::from_bytes(&good[..n]).is_err());
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(FontIndex::from_bytes(&trailing).is_err());
        // Bit flips never panic.
        for i in 0..good.len() {
            let mut b = good.clone();
            b[i] ^= 0xFF;
            let _ = FontIndex::from_bytes(&b);
        }
    }

    #[test]
    fn huge_counts_are_bounded() {
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&INDEX_VERSION.to_le_bytes());
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(FontIndex::from_bytes(&b), Err(IndexError::Corrupt));
    }

    #[test]
    fn garbage_font_bytes_index_to_nothing() {
        assert!(index_font_bytes(&[]).is_empty());
        assert!(index_font_bytes(b"OTTO\0\0\0\0\0\0\0\0\0\0\0\0").is_empty());
        assert!(index_font_bytes(&[0xFF; 4096]).is_empty());
    }

    #[test]
    fn faces_covering_is_range_lookup() {
        let i = sample();
        assert_eq!(i.faces_covering(0xE0B0).count(), 1);
        assert_eq!(i.faces_covering(0x3042).count(), 0);
    }
}
