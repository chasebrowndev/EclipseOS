// SPDX-License-Identifier: AGPL-3.0-only
//! The custom-widget runner: `bar { widget "<name>" { … } }` (ADR 0065).
//!
//! Runs each exec widget's command out of process, argv-exec and never
//! through an implicit shell, on this service's thread rather than a draw
//! path, with a timeout and a 4 KiB line cap, and kills it on reload or
//! removal. A command has exactly the authority of the user who wrote it into
//! their own `abyss.kdl`; this is not a plugin mechanism.
//!
//! Output is untrusted text: rendered plain, never as markup, and never
//! logged, since it may carry anything the command prints.
//!
//! `Source`-kind widgets are resolved by the GUI from its own feeds
//! ([`crate::usage`], [`crate::audio`], …); the runner ignores them.
//!
//! Skeleton: the thread only keeps the channels alive. The runner body lands
//! with the custom node of the taskbar-widgets work.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

/// One `widget` block, as read from `get_config`'s `collections.widget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetSpec {
    /// The block's name; the bar refers to it as `custom:<name>`.
    pub name: String,
    pub kind: Kind,
    /// Icon name or path.
    pub icon: Option<String>,
    pub on_click: Option<Vec<String>>,
    pub on_scroll_up: Option<Vec<String>>,
    pub on_scroll_down: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Run `argv` every `interval`; its output is one update.
    Exec { argv: Vec<String>, interval: Duration },
    /// Run `argv` once and keep it running; each line is one update.
    Stream { argv: Vec<String> },
    /// A shipped data source rendered through `format`. Resolved by the GUI.
    Source { source: String, format: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Output {
        name: String,
        out: Output,
    },
    /// The command could not run, timed out, or printed something unusable.
    /// `reason` is ours, never the command's output.
    Failed {
        name: String,
        reason: String,
    },
}

/// One update from a command: a plain-text line, or a JSON line
/// `{text, detail, tooltip, state}`. Untrusted; never markup, never logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub text: String,
    /// Lines for the widget's revealed section.
    pub detail: Vec<String>,
    pub tooltip: Option<String>,
    /// A free-form state tag the GUI may style (e.g. `warning`).
    pub state: Option<String>,
}

// Read by the service body, which lands with its own node; `expect` fails
// the build the moment it does, so this cannot outlive the skeleton.
#[expect(dead_code, reason = "skeleton: the service body reads these")]
#[derive(Debug)]
enum Command {
    Reconfigure(Vec<WidgetSpec>),
    Run(Vec<String>),
}

/// The GUI's end of the runner.
pub struct Handle {
    updates: Receiver<Update>,
    commands: Sender<Command>,
}

impl Handle {
    /// The next update, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// Replace the widget set. Commands for removed or changed widgets are
    /// killed.
    pub fn reconfigure(&self, specs: Vec<WidgetSpec>) {
        let _ = self.commands.send(Command::Reconfigure(specs));
    }

    /// Fire-and-forget one argv (an `on-click` or `on-scroll-*` handler).
    pub fn run(&self, argv: Vec<String>) {
        let _ = self.commands.send(Command::Run(argv));
    }
}

/// Start the runner. The thread lives until the [`Handle`] is dropped.
pub fn spawn(specs: Vec<WidgetSpec>) -> Handle {
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("eclipse-custom".into())
        .spawn(move || run(specs, updates_tx, commands_rx));
    Handle { updates, commands }
}

fn run(_specs: Vec<WidgetSpec>, _updates: Sender<Update>, commands: Receiver<Command>) {
    for _command in commands {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_starts_quiet() {
        let spec = WidgetSpec {
            name: "weather".into(),
            kind: Kind::Exec {
                argv: vec!["true".into()],
                interval: Duration::from_secs(600),
            },
            icon: None,
            on_click: None,
            on_scroll_up: None,
            on_scroll_down: None,
        };
        let h = spawn(vec![spec.clone()]);
        assert_eq!(h.try_recv(), None);
        h.reconfigure(vec![spec]);
        h.run(vec!["true".into()]);
        assert_eq!(h.try_recv(), None);
    }
}
