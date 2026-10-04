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

use iced::widget::{column, row, text, Space};
use iced::{Alignment, Element, Length, Theme};

use ec_ui::theme;
use ec_ui::tokens::{font, size, space};
use ec_ui::widget::{micro_label, panel, pill, status_cell, status_grid};

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

/// The names a `capture { allow … }` line may grant the daemon under: its
/// executable, and the add-on id the spec still spells it as.
const GRANTED_AS: [&str; 2] = ["ec-oracle-eyes", ADDON];

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
    grant_in(policy.capture_allow.entries.iter().map(|e| e.name.as_str()), read)
}

/// `allowed` is every name on the merged `capture { allow … }` list; `read`
/// whether any policy file parsed at all.
fn grant_in<'a>(mut allowed: impl Iterator<Item = &'a str>, read: bool) -> Grant {
    if allowed.any(|n| GRANTED_AS.contains(&n)) {
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
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grant_is_read_by_executable_name() {
        assert_eq!(grant_in(["ec-oracle-eyes"].into_iter(), true), Grant::Granted);
        assert_eq!(grant_in(["ec-hyperion-bar"].into_iter(), true), Grant::NotGranted);
        assert_eq!(grant_in(std::iter::empty(), false), Grant::Unread);
    }
}
