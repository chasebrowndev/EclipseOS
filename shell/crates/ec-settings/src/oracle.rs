// SPDX-License-Identifier: AGPL-3.0-only
//! The Oracle Eyes section's hero: whether the add-on may capture the screen,
//! and the door to the policy viewer that shows why.
//!
//! The capture grant is read from `policy.kdl` on disk, read-only, with the
//! policy viewer's own reader — never written here and never asked of the
//! socket. The grant is owned by the policy editor; this pane only reports it.
//!
//! Everything below the hero (model, timing, debug, colours) is ordinary
//! schema rows from the `oracle-eyes` and `annotations` blocks of abyss.kdl.

use std::path::{Path, PathBuf};

use iced::widget::{column, row, text, Space};
use iced::{Alignment, Element, Length, Theme};

use ec_ui::theme;
use ec_ui::tokens::{font, size, space};
use ec_ui::widget::{glass_panel as panel, micro_label, pill, status_cell, status_grid};

use crate::app::{App, Message};

/// The add-on's id in its manifest and the add-on list.
pub const ADDON: &str = "oracle-eyes";

/// The model command's key. Withheld by the compositor until a change is
/// approved; its row says which (`Row::approval`).
pub const MODEL_COMMAND: &str = "oracle-eyes.model-command";

/// The caption under the model command: whether the command shown is the
/// one that runs.
pub fn approval_line(key: &crate::schema::Row) -> &'static str {
    match key.approval.as_deref() {
        Some("pending") => {
            "Awaiting approval in the compositor's prompt. Until you approve it, the last approved \
             command runs."
        }
        _ => "Approved. A changed command runs only once you approve it in the compositor's prompt.",
    }
}

/// Where the daemon may be installed, in the order a launch finds it: the dev
/// layout's `/usr/local/bin` symlink (`packaging/install-session.sh` rewrites
/// the unit's `ExecStart` to it), then the package's `/usr/bin`.
const DAEMON_PATHS: [&str; 2] = ["/usr/local/bin/ec-oracle-eyes", "/usr/bin/ec-oracle-eyes"];

/// What `policy.kdl` says about Oracle Eyes capturing the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    Granted,
    NotGranted,
    /// No policy file could be read at all.
    Unread,
}

impl Grant {
    pub fn as_str(self) -> &'static str {
        match self {
            Grant::Granted => "granted",
            Grant::NotGranted => "not granted",
            Grant::Unread => "not read",
        }
    }
}

/// Read the grant off disk. Cheap: two small files, read when the app
/// reloads, never per frame.
pub fn read_grant() -> Grant {
    let policy = ec_policy_viewer::read::load();
    let read = policy
        .files
        .iter()
        .any(|f| matches!(f.state, ec_policy_viewer::read::FileState::Read));
    grant_in(
        policy.capture_allow.entries.iter().map(|e| e.name.as_str()),
        &daemon_exe(),
        read,
    )
}

/// The daemon's executable as the compositor sees it: `/proc/<pid>/exe` is
/// canonical, so the first installed candidate with its symlinks resolved.
/// Installed nowhere: the package path, which is where an install puts it.
fn daemon_exe() -> PathBuf {
    DAEMON_PATHS
        .iter()
        .find_map(|p| std::fs::canonicalize(p).ok())
        .unwrap_or_else(|| PathBuf::from(DAEMON_PATHS[1]))
}

/// Whether one `capture.allow` entry admits `exe`, by the compositor's own
/// rule (ADR 0094): a bare name is `/usr/bin/<name>`, an entry with a slash is
/// that exact absolute path, and the entry is resolved through symlinks, kept
/// literal when it names no file. A malformed entry admits nothing.
fn admits(entry: &str, exe: &Path) -> bool {
    ec_abyss_config::exe_entry_path(entry)
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .is_some_and(|p| p == exe)
}

/// `allowed` is every entry on the merged `capture { allow … }` list, `exe`
/// the daemon's resolved executable, `read` whether any policy file parsed.
fn grant_in<'a>(mut allowed: impl Iterator<Item = &'a str>, exe: &Path, read: bool) -> Grant {
    if allowed.any(|n| admits(n, exe)) {
        Grant::Granted
    } else if read {
        Grant::NotGranted
    } else {
        Grant::Unread
    }
}

/// The header's two-line status chip.
pub fn status(app: &App) -> (String, String) {
    let debug = app
        .key("oracle-eyes.debug")
        .is_some_and(crate::schema::Row::as_bool);
    (
        format!("capture {}", app.oe_grant.as_str()),
        if debug { "debug mode on" } else { "debug mode off" }.into(),
    )
}

// Accent ledger: the one live yellow is the capture state when it is
// `granted` — the one thing on this page that is currently allowing
// something. Debug and the hook only report; the rows below have no accent
// but a picker's own swatch.

/// The hero: a status grid of what is true right now, with the door to the
/// policy viewer in its heading row.
pub fn hero(app: &App) -> Element<'_, Message, Theme> {
    let hook_on = app.addons.as_ref().is_some_and(|a| a.is_on("annotations"));
    let installed = app
        .addons
        .as_ref()
        .is_some_and(|a| a.addons.iter().any(|x| x.id == ADDON));
    let debug = app
        .key("oracle-eyes.debug")
        .is_some_and(crate::schema::Row::as_bool);
    let cells = vec![
        status_cell(
            "screen capture",
            app.oe_grant.as_str(),
            "requests screen capture \u{b7} policy.kdl",
            app.oe_grant == Grant::Granted,
        ),
        status_cell(
            "add-on",
            if installed { "installed" } else { "not installed" },
            if hook_on {
                "annotations hook on"
            } else {
                "annotations hook off"
            },
            false,
        ),
        status_cell(
            "debug mode",
            if debug { "on" } else { "off" },
            "red eye, shown in captures",
            false,
        ),
    ];
    panel(
        app.glass_radius,
        app.hairline_cards,
        column![
            row![
                micro_label("permission"),
                Space::new().width(Length::Fill),
                pill("Open policy viewer", false, Message::OpenPolicyViewer),
            ]
            .align_y(Alignment::Center),
            status_grid(cells, 3),
            text(
                "The grant is a line in policy.kdl, owned by the policy editor. Settings only \
                 reads it."
            )
            .font(font::UI)
            .size(size::BODY_SMALL)
            .style(theme::text_tertiary),
        ]
        .spacing(space::ROW_Y),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/usr/bin/ec-oracle-eyes` as the compositor would compare it on this
    /// host: resolved if it exists, literal if not.
    fn packaged() -> PathBuf {
        std::fs::canonicalize(DAEMON_PATHS[1]).unwrap_or_else(|_| DAEMON_PATHS[1].into())
    }

    fn grant(entries: &[&str], exe: &Path) -> Grant {
        grant_in(entries.iter().copied(), exe, true)
    }

    #[test]
    fn a_bare_name_means_usr_bin() {
        let exe = packaged();
        assert_eq!(grant(&["ec-oracle-eyes"], &exe), Grant::Granted);
        assert_eq!(grant(&["/usr/bin/ec-oracle-eyes"], &exe), Grant::Granted);
        assert_eq!(grant(&["ec-hyperion-bar"], &exe), Grant::NotGranted);
        // The same basename anywhere else is not the packaged daemon.
        assert_eq!(
            grant(&["ec-oracle-eyes"], Path::new("/tmp/ec-oracle-eyes")),
            Grant::NotGranted
        );
    }

    #[test]
    fn the_add_on_id_is_not_the_executable() {
        // `oracle-eyes` now means `/usr/bin/oracle-eyes`, which is not the daemon.
        assert_eq!(grant(&["oracle-eyes"], &packaged()), Grant::NotGranted);
    }

    #[test]
    fn a_malformed_entry_admits_nothing() {
        for bad in ["", ".", "..", "bin/ec-oracle-eyes"] {
            assert_eq!(grant(&[bad], &packaged()), Grant::NotGranted, "{bad:?}");
        }
    }

    #[test]
    fn a_symlink_entry_admits_the_file_it_points_to() {
        let dir = std::env::temp_dir().join(format!("ec-settings-oracle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("ec-oracle-eyes");
        let link = dir.join("link");
        std::fs::write(&real, b"").unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let exe = std::fs::canonicalize(&real).unwrap();
        let entry = link.to_str().unwrap();
        assert_eq!(grant(&[entry], &exe), Grant::Granted);
        // A dev symlink entry does not admit the packaged daemon.
        assert_eq!(grant(&[entry], &packaged()), Grant::NotGranted);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nothing_read_is_unread_not_denied() {
        assert_eq!(grant_in(std::iter::empty(), &packaged(), false), Grant::Unread);
        assert_eq!(grant(&[], &packaged()), Grant::NotGranted);
    }
}
