// SPDX-License-Identifier: AGPL-3.0-only
//! Quick actions: a grid of one-tap buttons from `bar { action "<id>" { … } }`
//! (ADR 0065 amendment). The cell is a single understated glyph, zero width
//! while no action is configured; clicking it opens the grid drawer.
//!
//! An `app` target launches its desktop entry the way the launcher does. An
//! `exec` target runs its argv, never through a shell, and only when the
//! owner's approval for exactly that argv is recorded (ADR 0067); until then
//! its tile is disabled and says so. The bar never runs argv it has not been
//! told is approved, and it checks again at the press, not only at load.
//!
//! The control socket does not serve `bar.quick_actions` (ADR 0067 keeps that
//! collection off the socket), so the bar reads the owner's own config file
//! through `ec-abyss-config`, the same parser the compositor uses, and the
//! approvals store the same way.

use std::process::Stdio;

use ec_abyss_config::approvals::{self, Approvals};
use ec_abyss_config::{QuickAction, QuickActionTarget};
use ec_ui::tokens::{bar, color, drawer};
use ec_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::view::{elide, Sheet};

/// The cell's glyph: a grid of squares, the usual "more actions" mark.
const CELL_ICON: &str = "view-app-grid";

/// How much of an action's label a tile shows. A tile is half the drawer.
const LABEL_CHARS: usize = 14;

/// One configured action, and what the bar may do with it right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub action: QuickAction,
    /// The press runs the action. `false` draws the tile disabled.
    pub runnable: bool,
    /// The reason a tile is disabled, in its mono line: "needs approval",
    /// "not installed". `None` for a runnable action.
    pub note: Option<&'static str>,
}

/// The configured actions, with their gates decided. Read at start and on
/// every reload, never from a draw. Empty under `cfg(test)`, which must not
/// depend on the machine's own config.
pub fn load() -> Vec<Slot> {
    if cfg!(test) {
        return Vec::new();
    }
    let actions = ec_abyss_config::Config::load(None).bar.quick_actions;
    if actions.is_empty() {
        return Vec::new();
    }
    let approvals = approvals::read();
    let entries = ec_services::apps::scan(None);
    actions
        .into_iter()
        .map(|action| {
            let installed = |id: &str| entries.iter().any(|e| same_entry(&e.id, id));
            let (runnable, note) = gate(&action, &approvals, installed);
            Slot {
                action,
                runnable,
                note,
            }
        })
        .collect()
}

/// Whether a press may run `action`, and the note its tile carries when not.
/// `installed` answers for a desktop-entry id. Pure, so the rule is tested
/// without a disk.
fn gate(
    action: &QuickAction,
    approvals: &Approvals,
    installed: impl Fn(&str) -> bool,
) -> (bool, Option<&'static str>) {
    match &action.target {
        QuickActionTarget::App(id) if installed(id) => (true, None),
        QuickActionTarget::App(_) => (false, Some("not installed")),
        QuickActionTarget::Exec(_) if action.runnable(approvals) => (true, None),
        QuickActionTarget::Exec(_) => (false, Some("needs approval")),
    }
}

/// A desktop-entry id as the config spells it: with or without `.desktop`.
fn same_entry(entry_id: &str, config_id: &str) -> bool {
    entry_id == config_id || entry_id.strip_suffix(".desktop") == Some(config_id)
}

pub fn spans(app: &App) -> Spans {
    let present = !app.quick_actions.is_empty();
    Spans {
        core: if present { bar::MARK } else { 0.0 },
        revealed: 0.0,
        lead: 0.0,
        present,
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    if app.quick_actions.is_empty() {
        return Parts::empty();
    }
    let ink = frame.core_alpha();
    let mark = parts::mark(
        crate::icons::symbolic(CELL_ICON),
        bar::MARK,
        color::text_secondary().scale_alpha(ink),
    );
    Parts {
        core: super::press(mark, bar::MARK, Message::Open(Drawer::QuickActions)),
        revealed: None,
    }
}

/// The quick actions drawer: one tile per action, two to a row, in the
/// order the config lists them. See `view::drawer_view` for the ledger: a
/// shelf of buttons carries no yellow at all.
pub(crate) fn sheet(app: &App) -> Sheet {
    let mut s = Sheet::new(parts::drawer_sheet_head("actions"), drawer::HEAD_H);
    if app.quick_actions.is_empty() {
        s.row(parts::drawer_note("add an action: docs/CONFIG.md"));
        return s;
    }
    let tiles: Vec<_> = app
        .quick_actions
        .iter()
        .enumerate()
        .map(|(index, slot)| {
            let mark = parts::mark(
                slot.action.icon.as_deref().and_then(crate::icons::symbolic),
                drawer::MARK,
                color::text_secondary(),
            );
            let label = elide(&slot.action.label, LABEL_CHARS);
            let press = slot.runnable.then_some(Message::QuickAction(index));
            parts::drawer_tile(mark, &label, slot.note, press)
        })
        .collect();
    s.block(
        parts::tile_grid(tiles),
        parts::tile_grid_height(app.quick_actions.len()),
    );
    s
}

/// Run what the press asked for. Refused, silently, unless the action is
/// runnable now: an `exec` whose approval is not recorded for exactly its
/// argv never starts, whatever the slot said when it was loaded.
pub fn run(slot: &Slot, term: Option<String>) {
    match &slot.action.target {
        QuickActionTarget::Exec(argv) => {
            if !slot.action.runnable(&approvals::read()) {
                return;
            }
            spawn_argv(argv);
        }
        QuickActionTarget::App(id) => {
            let entries = ec_services::apps::scan(None);
            if let Some(entry) = entries.iter().find(|e| same_entry(&e.id, id)) {
                let _ = ec_services::apps::launch(entry, term.as_deref());
            }
        }
    }
}

/// Start `argv` directly, with no shell, detached from the bar's streams. The
/// child is waited on by its own thread, so the bar keeps no zombie and never
/// blocks on the command. The argv is never logged: it may carry anything.
fn spawn_argv(argv: &[String]) {
    let Some((program, args)) = argv.split_first() else {
        return;
    };
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = std::thread::Builder::new()
        .name("eclipse-quick-action".to_owned())
        .spawn(move || {
            if let Ok(mut child) = command.spawn() {
                let _ = child.wait();
            }
        });
}

/// The fixture's actions for `HYPERION_PREVIEW=quick-actions`: two that run,
/// one awaiting approval, one whose entry is not installed, and one more
/// awaiting approval. Debug builds only; a screenshot needs no config and no
/// approval.
#[cfg(debug_assertions)]
pub fn preview_slots() -> Vec<Slot> {
    let slot = |id: &str, label: &str, icon: &str, target: QuickActionTarget, runnable: bool, note| Slot {
        action: QuickAction {
            id: id.to_owned(),
            label: label.to_owned(),
            icon: Some(icon.to_owned()),
            target,
        },
        runnable,
        note,
    };
    let app = |id: &str| QuickActionTarget::App(id.to_owned());
    let exec = |argv: &[&str]| QuickActionTarget::Exec(argv.iter().map(|a| (*a).to_owned()).collect());
    vec![
        slot(
            "terminal",
            "Terminal",
            "utilities-terminal",
            app("foot.desktop"),
            true,
            None,
        ),
        slot(
            "files",
            "Files",
            "system-file-manager",
            app("org.gnome.Nautilus.desktop"),
            true,
            None,
        ),
        slot(
            "mute-mic",
            "Mute mic",
            "audio-input-microphone",
            exec(&["pactl", "set-source-mute", "@DEFAULT_SOURCE@", "toggle"]),
            false,
            Some("needs approval"),
        ),
        slot(
            "notes",
            "Notes",
            "accessories-text-editor",
            app("missing.desktop"),
            false,
            Some("not installed"),
        ),
        slot(
            "lock",
            "Lock",
            "system-lock-screen",
            exec(&["loginctl", "lock-session"]),
            false,
            Some("needs approval"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(id: &str, target: QuickActionTarget) -> QuickAction {
        QuickAction {
            id: id.to_owned(),
            label: id.to_owned(),
            icon: None,
            target,
        }
    }

    #[test]
    fn an_exec_without_an_approval_is_disabled_and_says_why() {
        let mute = action(
            "mute",
            QuickActionTarget::Exec(vec![
                "pactl".into(),
                "set-sink-mute".into(),
                "@DEFAULT_SINK@".into(),
                "toggle".into(),
            ]),
        );
        assert_eq!(
            gate(&mute, &Approvals::default(), |_| true),
            (false, Some("needs approval"))
        );
    }

    #[test]
    fn an_app_runs_only_when_its_entry_is_installed() {
        let term = action("term", QuickActionTarget::App("org.foo.Term.desktop".into()));
        assert_eq!(gate(&term, &Approvals::default(), |_| true), (true, None));
        assert_eq!(
            gate(&term, &Approvals::default(), |_| false),
            (false, Some("not installed"))
        );
    }

    #[test]
    fn a_config_id_matches_its_desktop_entry_with_or_without_the_suffix() {
        assert!(same_entry("org.foo.Term.desktop", "org.foo.Term"));
        assert!(same_entry("org.foo.Term.desktop", "org.foo.Term.desktop"));
        assert!(!same_entry("org.foo.Term.desktop", "org.foo"));
    }
}
