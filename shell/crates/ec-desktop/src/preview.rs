// SPDX-License-Identifier: AGPL-3.0-only
//! A debug-build fixture for screenshotting the desktop on a machine with
//! nothing on its desktop and nothing that can click, modelled on the bar's
//! `HYPERION_PREVIEW`.
//!
//! `DESKTOP_PREVIEW=icons` fills a fake desktop (folders, files, a launcher
//! that is not allowed to run, a broken link, a long name), selects one icon
//! and opens the empty-space menu. `DESKTOP_PREVIEW=prompt` opens the launch
//! prompt instead, `=icon-menu` the menu on an icon, `=rename` the rename field,
//! `=toast` a failure note. The fixture lives under a scratch directory and
//! saves nothing to the real layout; it is read once at start.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use iced::Point;

use crate::app::App;
use crate::desktop::{Desktop, Failure};
use crate::scan::{Entry, Kind, Launcher};
use crate::ui::{Menu, Target};

fn entry(dir: &Path, name: &str, kind: Kind, mime: Option<&'static str>) -> Entry {
    Entry {
        name: name.to_owned(),
        path: dir.join(name),
        kind,
        mime,
        modified: Some(SystemTime::UNIX_EPOCH),
        size: 1024,
        to_dir: false,
        broken: false,
        launcher: None,
    }
}

/// The fake desktop's entries, in directory order.
pub fn fixture(dir: &Path) -> Vec<Entry> {
    let mut v = vec![
        entry(dir, "Documents", Kind::Dir, None),
        entry(dir, "Projects", Kind::Dir, None),
        entry(dir, "Screenshots", Kind::Dir, None),
        entry(
            dir,
            "budget-2026.ods",
            Kind::File,
            Some("application/vnd.oasis.opendocument.spreadsheet"),
        ),
        entry(dir, "notes.txt", Kind::File, Some("text/plain")),
        entry(dir, "holiday.png", Kind::File, Some("image/png")),
        entry(dir, "track-07.flac", Kind::File, Some("audio/flac")),
        entry(dir, "archive.tar.gz", Kind::File, Some("application/gzip")),
        entry(
            dir,
            "an-extraordinarily-long-file-name-that-keeps-going-and-going.txt",
            Kind::File,
            Some("text/plain"),
        ),
    ];
    let mut app = entry(
        dir,
        "setup-tool.desktop",
        Kind::Launcher,
        Some("application/x-desktop"),
    );
    app.launcher = Some(Launcher {
        name: "Setup Tool".to_owned(),
        icon: None,
        app: None,
        trusted: false,
    });
    v.push(app);
    let mut link = entry(dir, "old-share", Kind::Symlink, None);
    link.broken = true;
    v.push(link);
    v
}

/// Replace the model's folder with the fixture and set the interface up as
/// `DESKTOP_PREVIEW` asks. Does nothing when the variable is unset.
pub fn apply(app: &mut App) {
    let Ok(which) = std::env::var("DESKTOP_PREVIEW") else {
        return;
    };
    let dir = PathBuf::from("/nonexistent/desktop-preview");
    // No folder: the watcher and every operation find nothing to touch.
    app.desktop = Desktop::with(None, None, None);
    app.desktop.entries = fixture(&dir);
    app.desktop.icons = app
        .desktop
        .entries
        .iter()
        .map(|e| {
            (
                e.name.clone(),
                crate::icons::for_entry(e, crate::desktop::ICON_LOOKUP),
            )
        })
        .collect();
    let names = ["Documents", "budget-2026.ods"];
    match which.as_str() {
        "icons" | "" => {
            app.ui.selected = names.iter().map(|s| (*s).to_owned()).collect();
            app.ui.cursor = Some("Documents".to_owned());
            app.ui.menu = Some(Menu {
                at: Point::new(520.0, 300.0),
                target: Target::Empty,
            });
        }
        "icon-menu" => {
            app.ui.selected.insert("notes.txt".to_owned());
            app.ui.menu = Some(Menu {
                at: Point::new(300.0, 160.0),
                target: Target::Icon("notes.txt".to_owned()),
            });
        }
        "prompt" => {
            app.ui.selected.insert("setup-tool.desktop".to_owned());
            app.ui.prompt = Some("setup-tool.desktop".to_owned());
        }
        "rename" => {
            app.ui.selected.insert("notes.txt".to_owned());
            app.ui.begin_rename("notes.txt");
        }
        "toast" => {
            app.ui.selected.insert("holiday.png".to_owned());
            app.ui.notify(Failure {
                op: "activate",
                kind: "not-allowed",
            });
        }
        _ => {}
    }
}
