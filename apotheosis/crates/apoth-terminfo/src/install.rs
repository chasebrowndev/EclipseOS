// SPDX-License-Identifier: Apache-2.0
//! The packaging hook: write the compiled entries into a terminfo directory tree.

use std::io;
use std::path::{Path, PathBuf};

use crate::{Variant, compile, entry};

/// Every file the packaging step installs, as (path relative to the terminfo root,
/// compiled bytes): `x/<name>` (letter directory) and `78/<name>` (hex directory, the
/// first byte of the name in lowercase hex) for both variants.
pub fn tree() -> Result<Vec<(PathBuf, Vec<u8>)>, crate::CompileError> {
    let mut files = Vec::new();
    for v in Variant::ALL {
        let name = v.term_name();
        let bytes = compile(&entry(v), None)?;
        let first = name.as_bytes()[0];
        for dir in [char::from(first).to_string(), format!("{first:02x}")] {
            files.push((Path::new(&dir).join(name), bytes.clone()));
        }
    }
    Ok(files)
}

/// Write [`tree`] under `root` (created if missing). Returns the paths written.
pub fn write_tree(root: &Path) -> io::Result<Vec<PathBuf>> {
    let files = tree().map_err(io::Error::other)?;
    let mut written = Vec::with_capacity(files.len());
    for (rel, bytes) in files {
        let path = root.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, bytes)?;
        written.push(path);
    }
    Ok(written)
}
