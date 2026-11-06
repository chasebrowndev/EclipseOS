// SPDX-License-Identifier: AGPL-3.0-only
//! The desktop model: the folder's entries, their icons, the saved layout, and
//! the one door every operation goes through ([`Desktop::run`]).
//!
//! No iced here. `app::App` owns one `Desktop`, the view reads it, and the
//! view's clicks and drops come back as [`Op`]s. Everything is tested against
//! scratch directories.
//!
//! Only paths directly on the desktop are operated on, and only the desktop or
//! one of its folders is a destination: an `Op` naming anything else is
//! refused with kind `outside` and does nothing. Failures are
//! [`Failure`]s (an operation word and an error kind); no name or path is
//! ever logged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::icons;
use crate::launch;
use crate::layout::{Cell, Grid, Layout, Sort};
use crate::ops;
use crate::scan::{self, Entry};
use crate::trash;

/// The nominal icon size looked up for [`Desktop::icons`].
pub const ICON_LOOKUP: u16 = 64;

/// What the view asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// A new folder (`name` is the first choice; a taken name gets ` (2)`).
    NewFolder {
        name: String,
    },
    NewFile {
        name: String,
    },
    /// Rename `path` to `name` (suffixed if taken).
    Rename {
        path: PathBuf,
        name: String,
    },
    /// Copy `src` into `into` (the desktop itself when `None`).
    Copy {
        src: PathBuf,
        into: Option<PathBuf>,
    },
    /// Move `src` into the folder `into`; `None` is the desktop (a no-op).
    Move {
        src: PathBuf,
        into: Option<PathBuf>,
    },
    /// Delete to the trash. There is no other delete.
    Trash {
        path: PathBuf,
    },
    /// Double-click: launch or open.
    Activate {
        path: PathBuf,
    },
    /// The human allows this `.desktop` file to launch.
    AllowLaunching {
        path: PathBuf,
    },
    /// Sort one output's icons.
    Sort {
        output: String,
        sort: Sort,
        grid: Grid,
    },
    /// Drop `name` on a cell of one output.
    Place {
        output: String,
        name: String,
        cell: Cell,
        grid: Grid,
    },
}

impl Op {
    fn word(&self) -> &'static str {
        match self {
            Op::NewFolder { .. } => "new-folder",
            Op::NewFile { .. } => "new-file",
            Op::Rename { .. } => "rename",
            Op::Copy { .. } => "copy",
            Op::Move { .. } => "move",
            Op::Trash { .. } => "trash",
            Op::Activate { .. } => "activate",
            Op::AllowLaunching { .. } => "allow-launching",
            Op::Sort { .. } => "sort",
            Op::Place { .. } => "place",
        }
    }
}

/// An operation that did nothing, by word and kind. `Display` is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    pub op: &'static str,
    pub kind: &'static str,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.op, self.kind)
    }
}

/// What a successful operation made, so the view can select or edit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    Nothing,
    /// A new or moved or renamed or copied item, at this path.
    Path(PathBuf),
    /// Dropped on this cell.
    Cell(Cell),
}

pub struct Desktop {
    pub dir: Option<PathBuf>,
    /// Directory order, as scanned.
    pub entries: Vec<Entry>,
    /// Icon file by entry name; `None` is a miss (the view draws its token
    /// placeholder).
    pub icons: HashMap<String, Option<PathBuf>>,
    pub layout: Layout,
    /// `misc.terminal-command`, for `Terminal=true` launchers.
    pub term: Option<String>,
    /// The last failed operation, for the view to show once and clear.
    pub failure: Option<Failure>,
    layout_path: Option<PathBuf>,
    data_home: Option<PathBuf>,
    /// Re-resolve the folder on each rescan (the real desktop), so a
    /// `user-dirs.dirs` written after login is followed.
    follow: bool,
}

impl Desktop {
    /// The real desktop: its folder from the environment, its saved layout.
    pub fn open() -> Desktop {
        let layout_path = crate::layout::state_path();
        let mut d = Desktop::with(scan::desktop_dir(), layout_path, trash::data_home());
        d.follow = true;
        d.rescan();
        d
    }

    pub fn with(dir: Option<PathBuf>, layout_path: Option<PathBuf>, data_home: Option<PathBuf>) -> Desktop {
        Desktop {
            dir,
            entries: Vec::new(),
            icons: HashMap::new(),
            layout: layout_path.as_deref().map(Layout::load).unwrap_or_default(),
            term: None,
            failure: None,
            layout_path,
            data_home,
            follow: false,
        }
    }

    /// Point the desktop at another folder. The saved layout is by name and
    /// stays; the entries and their icons are the old folder's and go.
    pub fn set_dir(&mut self, dir: Option<PathBuf>) {
        if dir != self.dir {
            self.dir = dir;
            self.entries.clear();
            self.icons.clear();
        }
    }

    /// Read the folder again. Returns whether anything the desktop draws
    /// moved. Looks up icons for new entries only.
    pub fn rescan(&mut self) -> bool {
        if self.follow {
            if let Some(now) = scan::desktop_dir() {
                self.set_dir(Some(now));
            }
        }
        let Some(dir) = self.dir.clone() else {
            return false;
        };
        // An unreadable folder (unmounted, mid-rename) is not an empty one:
        // keep what is shown and, above all, the saved positions.
        let Some(next) = scan::try_scan(&dir, self.term.as_deref()) else {
            return false;
        };
        if next == self.entries {
            return false;
        }
        self.icons.retain(|n, _| next.iter().any(|e| &e.name == n));
        for e in &next {
            let stale = self.entries.iter().find(|o| o.name == e.name) != Some(e);
            if stale || !self.icons.contains_key(&e.name) {
                self.icons
                    .insert(e.name.clone(), icons::for_entry(e, ICON_LOOKUP));
            }
        }
        self.entries = next;
        let before = self.layout.clone();
        self.layout.prune(&self.entries);
        if self.layout != before {
            self.save_layout();
        }
        eprintln!("ec-desktop: {} entries", self.entries.len());
        true
    }

    /// Does `p` sit directly on the desktop?
    fn on_desktop(&self, p: &Path) -> bool {
        self.dir.as_deref().is_some_and(|d| p.parent() == Some(d))
    }

    /// A destination: the desktop itself, or one of its folders.
    fn destination(&self, into: Option<&Path>) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        match into {
            None => Some(dir.clone()),
            Some(p) if p == dir => Some(dir.clone()),
            // A real folder: a symlink to one would move the file out of the desktop.
            Some(p) if self.on_desktop(p) && is_real_dir(p) => Some(p.to_path_buf()),
            Some(_) => None,
        }
    }

    fn save_layout(&self) {
        if let Some(path) = &self.layout_path {
            if let Err(e) = self.layout.save(path) {
                eprintln!(
                    "ec-desktop: warning: layout not saved: {}",
                    ops::Error::from(e).kind()
                );
            }
        }
    }

    /// Run one operation, then rescan. A failure is logged by kind, kept in
    /// [`Desktop::failure`], and returned; nothing is half-reported.
    pub fn run(&mut self, op: Op) -> Result<Done, Failure> {
        let result = self.dispatch(op);
        match &result {
            Ok(_) => self.failure = None,
            Err(f) => {
                eprintln!("ec-desktop: warning: {f}");
                self.failure = Some(*f);
            }
        }
        self.rescan();
        result
    }

    fn dispatch(&mut self, op: Op) -> Result<Done, Failure> {
        let word = op.word();
        let fail = |kind: &'static str| Failure { op: word, kind };
        let from_ops = |e: ops::Error| Failure {
            op: word,
            kind: e.kind(),
        };
        let dir = self.dir.clone().ok_or(fail("no-desktop"))?;
        match op {
            Op::NewFolder { name } => ops::new_folder(&dir, &name).map(Done::Path).map_err(from_ops),
            Op::NewFile { name } => ops::new_file(&dir, &name).map(Done::Path).map_err(from_ops),
            Op::Rename { path, name } => {
                if !self.on_desktop(&path) {
                    return Err(fail("outside"));
                }
                let old = file_name(&path);
                let to = ops::rename(&path, &name).map_err(from_ops)?;
                self.layout.rename(&old, &file_name(&to));
                self.save_layout();
                Ok(Done::Path(to))
            }
            Op::Copy { src, into } => {
                let dest = self.destination(into.as_deref()).ok_or(fail("outside"))?;
                if !self.on_desktop(&src) {
                    return Err(fail("outside"));
                }
                ops::copy(&src, &dest).map(Done::Path).map_err(from_ops)
            }
            Op::Move { src, into } => {
                let dest = self.destination(into.as_deref()).ok_or(fail("outside"))?;
                if !self.on_desktop(&src) {
                    return Err(fail("outside"));
                }
                ops::move_to(&src, &dest).map(Done::Path).map_err(from_ops)
            }
            Op::Trash { path } => {
                if !self.on_desktop(&path) {
                    return Err(fail("outside"));
                }
                let data = self.data_home.clone().ok_or(fail("no-trash"))?;
                trash::trash(&data, &path).map(Done::Path).map_err(from_ops)
            }
            Op::Activate { path } => {
                let entry = self
                    .entries
                    .iter()
                    .find(|e| e.path == path)
                    .ok_or(fail("outside"))?;
                launch::activate(entry, self.term.as_deref())
                    .map(|()| Done::Nothing)
                    .map_err(|e| fail(e.kind()))
            }
            Op::AllowLaunching { path } => {
                if !self.on_desktop(&path) {
                    return Err(fail("outside"));
                }
                ops::allow_launching(&path)
                    .map(|()| Done::Nothing)
                    .map_err(from_ops)
            }
            Op::Sort { output, sort, grid } => {
                self.layout.sort(&output, sort, &self.entries, grid);
                self.save_layout();
                Ok(Done::Nothing)
            }
            Op::Place {
                output,
                name,
                cell,
                grid,
            } => {
                if !self.entries.iter().any(|e| e.name == name) {
                    return Err(fail("missing"));
                }
                let landed = self.layout.place(&output, &name, cell, &self.entries, grid);
                self.save_layout();
                Ok(Done::Cell(landed))
            }
        }
    }

    /// The cell of each entry on `output`, aligned with [`Desktop::entries`].
    pub fn cells(&self, output: &str, grid: Grid) -> Vec<Cell> {
        self.layout.arrange(output, &self.entries, grid)
    }
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmp;

    const GRID: Grid = Grid { cols: 8, rows: 4 };

    struct Fx {
        root: PathBuf,
        desk: Desktop,
    }

    fn fx() -> Fx {
        let root = tmp("desktop");
        let dir = root.join("Desktop");
        std::fs::create_dir(&dir).unwrap();
        let mut desk = Desktop::with(
            Some(dir),
            Some(root.join("state/eclipse/desktop-layout")),
            Some(root.join("data")),
        );
        desk.rescan();
        Fx { root, desk }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Fx {
        fn dir(&self) -> PathBuf {
            self.desk.dir.clone().unwrap()
        }
        fn names(&self) -> Vec<String> {
            let mut n: Vec<_> = self.desk.entries.iter().map(|e| e.name.clone()).collect();
            n.sort();
            n
        }
    }

    #[test]
    fn creating_renaming_and_the_rescan_after_each() {
        let mut f = fx();
        let made = f.desk.run(Op::NewFolder { name: "Stuff".into() }).unwrap();
        assert_eq!(made, Done::Path(f.dir().join("Stuff")));
        f.desk.run(Op::NewFile { name: "n.txt".into() }).unwrap();
        f.desk.run(Op::NewFile { name: "n.txt".into() }).unwrap();
        assert_eq!(f.names(), ["Stuff", "n (2).txt", "n.txt"]);
        let moved = f.desk.run(Op::Rename {
            path: f.dir().join("n.txt"),
            name: "m.txt".into(),
        });
        assert_eq!(moved, Ok(Done::Path(f.dir().join("m.txt"))));
        assert_eq!(f.names(), ["Stuff", "m.txt", "n (2).txt"]);
    }

    #[test]
    fn a_rename_carries_the_saved_position() {
        let mut f = fx();
        f.desk.run(Op::NewFile { name: "a".into() }).unwrap();
        let landed = f
            .desk
            .run(Op::Place {
                output: "DP-1".into(),
                name: "a".into(),
                cell: Cell { col: 3, row: 2 },
                grid: GRID,
            })
            .unwrap();
        assert_eq!(landed, Done::Cell(Cell { col: 3, row: 2 }));
        f.desk
            .run(Op::Rename {
                path: f.dir().join("a"),
                name: "b".into(),
            })
            .unwrap();
        let cells = f.desk.cells("DP-1", GRID);
        assert_eq!(cells, [Cell { col: 3, row: 2 }]);
        // And it was saved: a fresh desktop on the same state sees it.
        let again = Desktop::with(
            f.desk.dir.clone(),
            Some(f.root.join("state/eclipse/desktop-layout")),
            None,
        );
        assert_eq!(again.layout.output("DP-1").cells["b"], Cell { col: 3, row: 2 });
    }

    #[test]
    fn drag_onto_a_folder_moves_and_copies_without_overwriting() {
        let mut f = fx();
        f.desk.run(Op::NewFolder { name: "box".into() }).unwrap();
        f.desk.run(Op::NewFile { name: "f".into() }).unwrap();
        std::fs::write(f.dir().join("box/f"), "inside").unwrap();
        let copied = f.desk.run(Op::Copy {
            src: f.dir().join("f"),
            into: Some(f.dir().join("box")),
        });
        assert_eq!(copied, Ok(Done::Path(f.dir().join("box/f (2)"))));
        let moved = f.desk.run(Op::Move {
            src: f.dir().join("f"),
            into: Some(f.dir().join("box")),
        });
        assert_eq!(moved, Ok(Done::Path(f.dir().join("box/f (3)"))));
        assert_eq!(std::fs::read_to_string(f.dir().join("box/f")).unwrap(), "inside");
        assert_eq!(f.names(), ["box"]);
        // Copy onto the desktop itself duplicates.
        let dup = f.desk.run(Op::Copy {
            src: f.dir().join("box"),
            into: None,
        });
        assert_eq!(dup, Ok(Done::Path(f.dir().join("box (2)"))));
    }

    #[test]
    fn trash_goes_to_the_trash_and_is_the_only_delete() {
        let mut f = fx();
        f.desk
            .run(Op::NewFile {
                name: "gone.txt".into(),
            })
            .unwrap();
        f.desk
            .run(Op::Trash {
                path: f.dir().join("gone.txt"),
            })
            .unwrap();
        assert!(f.names().is_empty());
        assert!(f.root.join("data/Trash/files/gone.txt").exists());
        assert!(f.root.join("data/Trash/info/gone.txt.trashinfo").exists());
    }

    #[test]
    fn nothing_off_the_desktop_is_touched() {
        let mut f = fx();
        let outside = f.root.join("secret");
        std::fs::write(&outside, "x").unwrap();
        for op in [
            Op::Trash {
                path: outside.clone(),
            },
            Op::Rename {
                path: outside.clone(),
                name: "y".into(),
            },
            Op::Copy {
                src: outside.clone(),
                into: None,
            },
            Op::Move {
                src: outside.clone(),
                into: None,
            },
            Op::AllowLaunching {
                path: outside.clone(),
            },
            Op::Activate {
                path: outside.clone(),
            },
        ] {
            let e = f.desk.run(op).unwrap_err();
            assert_eq!(e.kind, "outside");
        }
        f.desk.run(Op::NewFile { name: "mine".into() }).unwrap();
        let e = f
            .desk
            .run(Op::Move {
                src: f.dir().join("mine"),
                into: Some(f.root.clone()),
            })
            .unwrap_err();
        assert_eq!(e.kind, "outside");
        assert!(outside.exists() && f.dir().join("mine").exists());
        assert_eq!(f.desk.failure, Some(e));
    }

    #[test]
    fn a_launcher_without_the_exec_bit_is_refused_until_allowed() {
        let mut f = fx();
        let p = f.dir().join("t.desktop");
        std::fs::write(&p, "[Desktop Entry]\nName=T\nExec=true\n").unwrap();
        f.desk.rescan();
        let e = f.desk.run(Op::Activate { path: p.clone() }).unwrap_err();
        assert_eq!(e.kind, "not-allowed");
        f.desk.run(Op::AllowLaunching { path: p.clone() }).unwrap();
        assert!(f.desk.entries[0].launcher.as_ref().unwrap().trusted);
    }

    #[test]
    fn an_unreadable_folder_keeps_the_layout_and_entries() {
        let mut f = fx();
        for n in ["a", "b"] {
            f.desk.run(Op::NewFile { name: n.into() }).unwrap();
        }
        f.desk
            .run(Op::Place {
                output: "DP-1".into(),
                name: "a".into(),
                cell: Cell { col: 3, row: 2 },
                grid: GRID,
            })
            .unwrap();
        let layout = f.desk.layout.clone();
        let saved = std::fs::read(f.root.join("state/eclipse/desktop-layout")).unwrap();
        let moved = f.root.join("away");
        std::fs::rename(f.dir(), &moved).unwrap();
        assert!(!f.desk.rescan());
        assert_eq!(f.desk.layout, layout);
        assert_eq!(f.names(), ["a", "b"]);
        assert_eq!(
            std::fs::read(f.root.join("state/eclipse/desktop-layout")).unwrap(),
            saved
        );
        std::fs::rename(&moved, f.dir()).unwrap();
        f.desk.rescan();
        assert_eq!(f.desk.layout, layout);
    }

    #[test]
    fn set_dir_shows_the_new_folder() {
        let mut f = fx();
        f.desk.run(Op::NewFile { name: "old".into() }).unwrap();
        let other = f.root.join("Skrivbord");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("new"), "").unwrap();
        f.desk.set_dir(Some(other));
        assert!(f.desk.rescan());
        assert_eq!(f.names(), ["new"]);
        assert!(f.desk.run(Op::NewFolder { name: "d".into() }).is_ok());
        assert!(f.root.join("Skrivbord/d").is_dir());
    }

    #[test]
    fn a_symlink_to_a_folder_is_not_a_destination() {
        let mut f = fx();
        let outside = f.root.join("Documents");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, f.dir().join("docs")).unwrap();
        f.desk.run(Op::NewFile { name: "m".into() }).unwrap();
        for op in [
            Op::Move {
                src: f.dir().join("m"),
                into: Some(f.dir().join("docs")),
            },
            Op::Copy {
                src: f.dir().join("m"),
                into: Some(f.dir().join("docs")),
            },
        ] {
            assert_eq!(f.desk.run(op).unwrap_err().kind, "outside");
        }
        assert!(f.dir().join("m").exists() && !outside.join("m").exists());
    }

    #[test]
    fn a_config_change_keeps_the_icons_it_has() {
        let mut f = fx();
        f.desk.run(Op::NewFile { name: "keep".into() }).unwrap();
        let p = f.dir().join("t.desktop");
        std::fs::write(&p, "[Desktop Entry]\nName=T\nExec=true\nTerminal=true\n").unwrap();
        f.desk.rescan();
        f.desk.icons.insert("keep".into(), Some("sentinel".into()));
        // The terminal moved: the Terminal=true launcher re-parses, nothing else.
        f.desk.term = Some("foot".into());
        assert!(f.desk.rescan());
        assert_eq!(f.desk.icons["keep"], Some(PathBuf::from("sentinel")));
        let l = f.desk.entries.iter().find(|e| e.name == "t.desktop").unwrap();
        assert!(l.launcher.as_ref().unwrap().app.is_some());
    }

    #[test]
    fn rescan_reports_change_only_when_something_moved() {
        let mut f = fx();
        assert!(!f.desk.rescan());
        std::fs::write(f.dir().join("x"), "1").unwrap();
        assert!(f.desk.rescan());
        assert!(f.desk.icons.contains_key("x"));
        assert!(!f.desk.rescan());
        std::fs::remove_file(f.dir().join("x")).unwrap();
        assert!(f.desk.rescan());
        assert!(f.desk.icons.is_empty());
    }

    #[test]
    fn sorting_is_saved_per_output() {
        let mut f = fx();
        for n in ["b", "a"] {
            f.desk.run(Op::NewFile { name: n.into() }).unwrap();
        }
        f.desk
            .run(Op::Sort {
                output: "DP-1".into(),
                sort: Sort::Name,
                grid: GRID,
            })
            .unwrap();
        let cells = f.desk.cells("DP-1", GRID);
        let at = |n: &str| cells[f.desk.entries.iter().position(|e| e.name == n).unwrap()];
        assert_eq!(at("a"), Cell { col: 0, row: 0 });
        assert_eq!(at("b"), Cell { col: 0, row: 1 });
        assert_eq!(f.desk.layout.output("HDMI-A-1").cells.len(), 0);
    }

    #[test]
    fn placing_a_missing_name_fails() {
        let mut f = fx();
        let e = f
            .desk
            .run(Op::Place {
                output: "o".into(),
                name: "nope".into(),
                cell: Cell { col: 0, row: 0 },
                grid: GRID,
            })
            .unwrap_err();
        assert_eq!(e.kind, "missing");
    }
}
