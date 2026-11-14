// SPDX-License-Identifier: AGPL-3.0-only
//! `mimeapps.list` and the default-application algorithm of the freedesktop "Association
//! between MIME types and applications" specification 1.0.1, for "Open with" (T-14 §11).
//!
//! - Files, most important first: `$XDG_CONFIG_HOME/{desktop-,}mimeapps.list`,
//!   `$XDG_CONFIG_DIRS/…`, `$XDG_DATA_HOME/applications/…`, `$XDG_DATA_DIRS/applications/…`.
//!   Desktop-specific files (`<desktop>-mimeapps.list`) carry only `[Default Applications]`.
//! - Associations: per directory, the file's `[Added Associations]` (unless blacklisted), then
//!   its `[Removed Associations]` join the blacklist, then that directory's desktop files
//!   whose `MimeType=` lists the type (unless blacklisted), then every desktop id of that
//!   directory is blacklisted. An addition naming an entry that does not exist at that level or
//!   lower is ignored.
//! - Default: the first id of the first `[Default Applications]` list (over all files in order)
//!   that is a valid installed entry associated with the type; else the most preferred
//!   associated entry. A more specific type is settled before a parent type is tried.

use std::collections::HashSet;
use std::path::PathBuf;

use super::desktop::{Entry, Index, split_list};

/// Largest `mimeapps.list` read.
pub(crate) const MAX_FILE: u64 = 256 * 1024;

/// One parsed `mimeapps.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct List {
    pub(crate) default: Vec<(String, Vec<String>)>,
    pub(crate) added: Vec<(String, Vec<String>)>,
    pub(crate) removed: Vec<(String, Vec<String>)>,
}

fn ids_for<'a>(sec: &'a [(String, Vec<String>)], mime: &str) -> impl Iterator<Item = &'a String> {
    sec.iter()
        .filter(move |(m, _)| m == mime)
        .flat_map(|(_, v)| v.iter())
}

impl List {
    /// Parse one file. A desktop-specific file keeps only its defaults.
    pub(crate) fn parse(text: &str, desktop_specific: bool) -> List {
        let mut l = List::default();
        #[derive(Clone, Copy, PartialEq)]
        enum Sec {
            None,
            Default,
            Added,
            Removed,
        }
        let mut sec = Sec::None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                sec = match line {
                    "[Default Applications]" => Sec::Default,
                    "[Added Associations]" if !desktop_specific => Sec::Added,
                    "[Removed Associations]" if !desktop_specific => Sec::Removed,
                    _ => Sec::None,
                };
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let entry = (k.trim().to_string(), split_list(v.trim()));
            match sec {
                Sec::Default => l.default.push(entry),
                Sec::Added => l.added.push(entry),
                Sec::Removed => l.removed.push(entry),
                Sec::None => {}
            }
        }
        l
    }
}

/// Where a list sits: its applications-directory level (`Some(i)`: the same directory as
/// `Index::dirs[i]`; `None`: a config directory, above every desktop-file directory).
#[derive(Clone, Debug)]
pub(crate) struct Located {
    pub(crate) level: Option<usize>,
    pub(crate) desktop_specific: bool,
    pub(crate) list: List,
}

/// The file locations in lookup order: `(path, level, desktop_specific)`.
pub(crate) fn locations(
    config_home: &std::path::Path,
    config_dirs: &[PathBuf],
    app_dirs: &[PathBuf],
    desktops: &[String],
) -> Vec<(PathBuf, Option<usize>, bool)> {
    let mut v = Vec::new();
    let mut add = |dir: &std::path::Path, level: Option<usize>| {
        for d in desktops {
            v.push((dir.join(format!("{d}-mimeapps.list")), level, true));
        }
        v.push((dir.join("mimeapps.list"), level, false));
    };
    add(config_home, None);
    for d in config_dirs {
        add(d, None);
    }
    for (i, d) in app_dirs.iter().enumerate() {
        add(d, Some(i));
    }
    v
}

/// The associations and default for MIME types.
#[derive(Debug)]
pub(crate) struct Resolver<'a> {
    pub(crate) index: &'a Index,
    pub(crate) lists: &'a [Located],
    /// `$PATH`, for `TryExec`.
    pub(crate) path: &'a str,
}

impl Resolver<'_> {
    fn valid(&self, e: &Entry) -> bool {
        !e.hidden
            && e.try_exec
                .as_deref()
                .is_none_or(|t| super::desktop::installed(t, self.path))
    }

    /// Entry ids associated with exactly `mime`, most preferred first.
    pub(crate) fn associated(&self, mime: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut black: HashSet<String> = HashSet::new();
        let levels = self.index.dirs.len();
        // Config-directory lists first, then each applications directory with its own list.
        let at = |lvl: Option<usize>| {
            self.lists
                .iter()
                .filter(move |l| l.level == lvl && !l.desktop_specific)
        };
        let exists_from = |id: &str, from: usize| {
            self.index.dirs[from..]
                .iter()
                .flatten()
                .any(|e| e.id == id && !e.hidden)
        };
        let mut visit = |lists: Vec<&Located>, lvl: Option<usize>, out: &mut Vec<String>| {
            let from = lvl.unwrap_or(0);
            for l in lists {
                for id in ids_for(&l.list.added, mime) {
                    if !black.contains(id) && exists_from(id, from.min(levels)) && !out.contains(id)
                    {
                        out.push(id.clone());
                    }
                }
                for id in ids_for(&l.list.removed, mime) {
                    black.insert(id.clone());
                }
            }
            if let Some(i) = lvl
                && let Some(dir) = self.index.dirs.get(i)
            {
                for e in dir {
                    if !e.hidden
                        && !black.contains(&e.id)
                        && e.mime.iter().any(|m| m == mime)
                        && !out.contains(&e.id)
                    {
                        out.push(e.id.clone());
                    }
                }
                for e in dir {
                    black.insert(e.id.clone());
                }
            }
        };
        visit(at(None).collect(), None, &mut out);
        for i in 0..levels {
            visit(at(Some(i)).collect(), Some(i), &mut out);
        }
        out.retain(|id| self.index.get(id).is_some_and(|e| self.valid(e)));
        out
    }

    /// The default entry id for exactly `mime`, if any.
    pub(crate) fn default_for(&self, mime: &str) -> Option<String> {
        let assoc = self.associated(mime);
        for l in self.lists {
            for id in ids_for(&l.list.default, mime) {
                if assoc.contains(id) {
                    return Some(id.clone());
                }
            }
        }
        assoc.into_iter().next()
    }

    /// The default for `mime` or, when it has none, its nearest ancestor that has one.
    pub(crate) fn default_through(&self, chain: &[String]) -> Option<String> {
        chain.iter().find_map(|m| self.default_for(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn entry(id: &str, mime: &[&str]) -> Entry {
        Entry {
            id: id.into(),
            path: PathBuf::from("/"),
            name: id.trim_end_matches(".desktop").into(),
            exec: "x %f".into(),
            try_exec: None,
            icon: None,
            mime: mime.iter().map(|s| (*s).to_string()).collect(),
            terminal: false,
            no_display: false,
            hidden: false,
            only_show_in: Vec::new(),
            not_show_in: Vec::new(),
        }
    }

    fn located(level: Option<usize>, ds: bool, text: &str) -> Located {
        Located {
            level,
            desktop_specific: ds,
            list: List::parse(text, ds),
        }
    }

    /// Level 0: `$XDG_DATA_HOME/applications`; level 1: `/usr/share/applications`.
    fn index() -> Index {
        Index {
            dirs: vec![
                vec![entry("mine.desktop", &["image/png"])],
                vec![
                    entry("loupe.desktop", &["image/png", "image/jpeg"]),
                    entry("gimp.desktop", &["image/png"]),
                    entry("firefox.desktop", &["text/html", "x-scheme-handler/https"]),
                    entry("editor.desktop", &["text/plain"]),
                ],
            ],
        }
    }

    #[test]
    fn parse_sections_and_desktop_specific_files() {
        let t = "[Default Applications]\nimage/png=a.desktop;b.desktop;\n\
                 [Added Associations]\nimage/png=c.desktop\n[Removed Associations]\nimage/png=d.desktop;\n\
                 [Other]\nx=y\n";
        let l = List::parse(t, false);
        assert_eq!(
            l.default,
            [(
                "image/png".into(),
                vec!["a.desktop".into(), "b.desktop".into()]
            )]
        );
        assert_eq!(l.added.len(), 1);
        assert_eq!(l.removed.len(), 1);
        let ds = List::parse(t, true);
        assert_eq!(ds.default.len(), 1);
        assert!(ds.added.is_empty() && ds.removed.is_empty());
    }

    #[test]
    fn locations_follow_the_lookup_order() {
        let v = locations(
            Path::new("/h/.config"),
            &[PathBuf::from("/etc/xdg")],
            &[
                PathBuf::from("/h/.local/share/applications"),
                PathBuf::from("/usr/share/applications"),
            ],
            &["abyss".into()],
        );
        let names: Vec<String> = v.iter().map(|(p, _, _)| p.display().to_string()).collect();
        assert_eq!(
            names,
            [
                "/h/.config/abyss-mimeapps.list",
                "/h/.config/mimeapps.list",
                "/etc/xdg/abyss-mimeapps.list",
                "/etc/xdg/mimeapps.list",
                "/h/.local/share/applications/abyss-mimeapps.list",
                "/h/.local/share/applications/mimeapps.list",
                "/usr/share/applications/abyss-mimeapps.list",
                "/usr/share/applications/mimeapps.list",
            ]
        );
    }

    #[test]
    fn associations_follow_directory_precedence() {
        let idx = index();
        let lists = [];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "",
        };
        assert_eq!(
            r.associated("image/png"),
            ["mine.desktop", "loupe.desktop", "gimp.desktop"]
        );
        assert_eq!(r.default_for("image/png").as_deref(), Some("mine.desktop"));
        assert_eq!(r.default_for("video/mp4"), None);
    }

    #[test]
    fn user_default_beats_system_default() {
        let idx = index();
        let lists = [
            located(
                None,
                false,
                "[Default Applications]\nimage/png=gimp.desktop\n",
            ),
            located(
                Some(1),
                false,
                "[Default Applications]\nimage/png=loupe.desktop\n",
            ),
        ];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "",
        };
        assert_eq!(r.default_for("image/png").as_deref(), Some("gimp.desktop"));
        // The desktop-specific user file comes first of all.
        let lists = [
            located(
                None,
                true,
                "[Default Applications]\nimage/png=loupe.desktop\n",
            ),
            located(
                None,
                false,
                "[Default Applications]\nimage/png=gimp.desktop\n",
            ),
        ];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "",
        };
        assert_eq!(r.default_for("image/png").as_deref(), Some("loupe.desktop"));
    }

    #[test]
    fn default_skips_missing_and_unassociated_ids() {
        let idx = index();
        let lists = [located(
            None,
            false,
            "[Default Applications]\nimage/png=gone.desktop;firefox.desktop;loupe.desktop\n",
        )];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "",
        };
        // gone: not installed; firefox: not associated with image/png.
        assert_eq!(r.default_for("image/png").as_deref(), Some("loupe.desktop"));
    }

    #[test]
    fn added_and_removed_associations() {
        let idx = index();
        let lists = [located(
            None,
            false,
            "[Added Associations]\nimage/png=editor.desktop;\n\
             [Removed Associations]\nimage/png=mine.desktop;loupe.desktop\n",
        )];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "",
        };
        // Added comes first (most preferred); removed entries are gone; a removed app set as
        // default is not used.
        assert_eq!(
            r.associated("image/png"),
            ["editor.desktop", "gimp.desktop"]
        );
        let lists = [
            located(
                None,
                false,
                "[Removed Associations]\nimage/png=loupe.desktop\n",
            ),
            located(
                None,
                false,
                "[Default Applications]\nimage/png=loupe.desktop\n",
            ),
        ];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "",
        };
        assert_eq!(r.default_for("image/png").as_deref(), Some("mine.desktop"));
    }

    #[test]
    fn a_system_removal_does_not_reach_a_user_copy() {
        // /usr/share/applications/mimeapps.list removes loupe, but the user has their own
        // loupe.desktop in $XDG_DATA_HOME: that copy keeps its association.
        let mut idx = index();
        idx.dirs[0].push(entry("loupe.desktop", &["image/png"]));
        let lists = [located(
            Some(1),
            false,
            "[Removed Associations]\nimage/png=loupe.desktop\n",
        )];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "",
        };
        assert!(
            r.associated("image/png")
                .contains(&"loupe.desktop".to_string())
        );
        // An addition in a lower directory for an id shadowed above is ignored.
        let lists = [located(
            Some(1),
            false,
            "[Added Associations]\nvideo/mp4=mine.desktop\n",
        )];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "",
        };
        assert!(r.associated("video/mp4").is_empty());
    }

    #[test]
    fn more_specific_type_first_then_parents() {
        let idx = index();
        let lists = [];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "",
        };
        let chain = ["text/x-rust".to_string(), "text/plain".to_string()];
        assert_eq!(r.default_through(&chain).as_deref(), Some("editor.desktop"));
    }

    #[test]
    fn try_exec_must_resolve() {
        let mut idx = index();
        idx.dirs[1][0].try_exec = Some("definitely-not-installed-xyz".into());
        let lists = [];
        let r = Resolver {
            index: &idx,
            lists: &lists,
            path: "/usr/bin",
        };
        assert!(
            !r.associated("image/jpeg")
                .contains(&"loupe.desktop".to_string())
        );
    }
}
