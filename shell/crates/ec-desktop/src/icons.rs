// SPDX-License-Identifier: AGPL-3.0-only
//! An entry's icon as a file on disk, from the freedesktop icon theme.
//!
//! Path-producing, as the bar's `icons.rs` is: it hands back a file and never
//! decodes one, so the view picks `svg` or `image` by extension. A miss is
//! `None` and the view draws its token placeholder, never a hole. The lookup
//! walks the disk, so callers do it once per entry when a scan arrives (cache
//! it on the model), never from the view.

use std::path::{Path, PathBuf};

use crate::mime;
use crate::scan::{Entry, Kind};

/// The theme the lookup starts from; the crate follows `Inherits=`.
const THEME: &str = "hicolor";
/// Asked second: it ships the standard `folder` and mime icons hicolor lacks.
const FALLBACK_THEME: &str = "Adwaita";

/// Icon names to try for `entry`, most specific first. A launcher's own
/// `Icon=` leads; folders are `folder`; files go by mime type.
pub fn names_for(entry: &Entry) -> Vec<String> {
    let mut names = Vec::new();
    match entry.kind {
        Kind::Launcher => {
            if let Some(icon) = entry.launcher.as_ref().and_then(|l| l.icon.clone()) {
                names.push(icon);
            }
            names.push("application-x-executable".to_owned());
        }
        Kind::Dir => names.push("folder".to_owned()),
        Kind::Symlink if entry.to_dir => names.push("folder".to_owned()),
        Kind::Symlink if entry.broken => names.push("dialog-warning".to_owned()),
        _ => names.extend(mime::icon_names(entry.mime)),
    }
    names
}

/// The file for one icon name (or an absolute path) at about `size` px.
pub fn resolve(name: &str, size: u16) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let direct = Path::new(name);
    if direct.is_absolute() {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    [THEME, FALLBACK_THEME].into_iter().find_map(|theme| {
        freedesktop_icons::lookup(name)
            .with_size(size)
            .with_theme(theme)
            .with_cache()
            .find()
    })
}

/// The first of [`names_for`] the theme has.
pub fn for_entry(entry: &Entry, size: u16) -> Option<PathBuf> {
    names_for(entry).iter().find_map(|n| resolve(n, size))
}

/// A menu verb's mark: the symbolic icon for `name` (`folder-new`), else the
/// plain one, else `None` and the view draws the placeholder.
pub fn symbolic(name: &str) -> Option<PathBuf> {
    resolve(&format!("{name}-symbolic"), MARK_LOOKUP).or_else(|| resolve(name, MARK_LOOKUP))
}

/// The size a menu mark is looked up at.
const MARK_LOOKUP: u16 = 16;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::parse_launcher;

    fn base(name: &str, kind: Kind) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from("/d").join(name),
            kind,
            mime: mime::of(name),
            modified: None,
            size: 0,
            to_dir: false,
            broken: false,
            launcher: None,
        }
    }

    #[test]
    fn names_lead_with_the_most_specific() {
        assert_eq!(names_for(&base("d", Kind::Dir)), ["folder"]);
        assert_eq!(names_for(&base("a.png", Kind::File))[0], "image-png");
        let mut l = base("a.desktop", Kind::Launcher);
        l.launcher = Some(parse_launcher(
            "a.desktop",
            "[Desktop Entry]\nName=A\nIcon=my-icon\n",
            0o755,
            None,
        ));
        assert_eq!(names_for(&l)[0], "my-icon");
        let mut dead = base("l", Kind::Symlink);
        dead.broken = true;
        assert_eq!(names_for(&dead), ["dialog-warning"]);
    }

    #[test]
    fn an_absolute_icon_must_exist_and_junk_is_none() {
        let d = crate::tmp("icons");
        let f = d.join("i.png");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(resolve(f.to_str().unwrap(), 48), Some(f));
        assert_eq!(resolve("/nonexistent/ec-desktop.png", 48), None);
        assert_eq!(resolve("", 48), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
