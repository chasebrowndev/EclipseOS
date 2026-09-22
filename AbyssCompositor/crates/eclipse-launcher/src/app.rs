// SPDX-License-Identifier: AGPL-3.0-only
//! Launcher state.
//!
//! The launcher is a menu like the control center: it is spawned by a keybind,
//! it answers one question — which application — and it goes away. It holds no
//! capability of its own (ADR 0038); `apps::launch` spawns a child of this
//! process, and when it refuses, the refusal is shown verbatim.

use iced::{Subscription, Task};
use iced_layershell::to_layer_message;

use eclipse_services::apps::{self, Entry};

/// The filter field. Named so the boot task can put the cursor in it before
/// the human has typed anything.
pub const INPUT_ID: &str = "launcher-query";

#[to_layer_message]
#[derive(Debug, Clone)]
pub enum Message {
    /// The filter field changed.
    Query(String),
    /// A row was pointed at.
    Select(usize),
    /// The selection moved by a row: -1 up, 1 down.
    Move(i32),
    /// Run whatever is selected.
    Activate,
    /// Dismiss without running anything.
    Close,
}

pub struct App {
    /// Every installed application, read once. A launcher that re-scanned
    /// `/usr/share/applications` on each keystroke would put a directory walk
    /// in the draw path for a set that changes when a package is installed.
    pub entries: Vec<Entry>,
    pub query: String,
    /// Indices into `entries`, best match first.
    pub matched: Vec<usize>,
    /// Index into `matched`, not into `entries`.
    pub selected: usize,
    /// `launch`'s own refusal — a terminal-only entry, or a failed spawn —
    /// shown as it came. Restating it in our words would be a worse answer.
    pub problem: Option<String>,
    /// `misc.terminal-command`, read once at startup (TERM-01). `None` means
    /// no terminal is configured — same as the socket being unreachable —
    /// and `Terminal=true` entries are dropped from `entries` rather than
    /// shown and then refused.
    term: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let term = crate::conn::fetch_terminal_command();
        let mut app = App {
            entries: apps::scan(term.as_deref()),
            query: String::new(),
            matched: Vec::new(),
            selected: 0,
            problem: None,
            term,
        };
        app.refilter();
        app
    }

    /// Rebuild `matched` for the current query and keep the selection inside
    /// it. Rank descending, then name ascending: a stable order, so the row
    /// under the pointer does not change identity between two keystrokes that
    /// rank the same.
    fn refilter(&mut self) {
        let query = self.query.clone();
        let mut matched: Vec<usize> = (0..self.entries.len())
            .filter(|&i| apps::matches(&self.entries[i], &query))
            .collect();
        matched.sort_by(|&a, &b| {
            apps::rank(&self.entries[b], &query)
                .cmp(&apps::rank(&self.entries[a], &query))
                .then_with(|| self.entries[a].name.cmp(&self.entries[b].name))
        });
        self.matched = matched;
        self.selected = self.selected.min(self.matched.len().saturating_sub(1));
    }

    /// The entry the human would run right now, if there is one.
    pub fn current(&self) -> Option<&Entry> {
        self.matched.get(self.selected).map(|&i| &self.entries[i])
    }
}

/// Leave. `iced::exit()` alone does not end an `iced_layershell` process —
/// the exit action is queued onto a loop that then sleeps waiting for a
/// Wayland event that never comes, so the dead launcher stays mapped and,
/// once the application it started takes focus, stops receiving the keys
/// that would wake it. A launcher holds no unsaved state and owns no
/// resource the compositor will not reclaim when the socket closes, so the
/// honest move is to go. Under `cfg(test)` this would take the test harness
/// with it, so there it is the plain action.
fn quit() -> Task<Message> {
    #[cfg(not(test))]
    std::process::exit(0);
    #[cfg(test)]
    iced::exit()
}

/// Put the caret back in the filter field.
///
/// Any pointer press inside the pane takes focus off the `text_input`, and
/// nothing gives it back on its own; every handler a press can reach returns
/// this.
fn refocus() -> Task<Message> {
    iced::widget::operation::focus(INPUT_ID)
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Query(query) => {
            app.query = query;
            // A new query is a new question; the old refusal was about a row
            // that may no longer be on screen.
            app.problem = None;
            // Typing moves the human back to the best match rather than
            // leaving the highlight on whatever row happens to be third.
            app.selected = 0;
            app.refilter();
        }
        Message::Select(index) => {
            if index < app.matched.len() {
                app.selected = index;
                // The old refusal was about the row we just left.
                app.problem = None;
            }
            // A hover arrives from the pointer, which may have pressed on the
            // way in and taken the caret out of the field with it.
            return refocus();
        }
        Message::Move(delta) => {
            app.problem = None;
            if app.matched.is_empty() {
                return Task::none();
            }
            let last = app.matched.len() - 1;
            // Clamped, not wrapped: a list that jumps from the top to the
            // bottom under a held arrow key is a list you overshoot.
            app.selected = (app.selected as i64 + delta as i64).clamp(0, last as i64) as usize;
        }
        Message::Activate => {
            let Some(entry) = app.current() else {
                return Task::none();
            };
            match apps::launch(entry, app.term.as_deref()) {
                // The application is running; the launcher has nothing left
                // to say.
                Ok(()) => return quit(),
                Err(error) => app.problem = Some(error.to_string()),
            }
            // We are still here, so the human is still searching. A press on
            // a row unfocuses the field, and a launcher whose prompt has
            // stopped accepting keystrokes while still looking live is worse
            // than one that closed.
            return refocus();
        }
        Message::Close => return quit(),
        // `to_layer_message` injects the layer-control variants. The surface
        // is a fixed size for its whole short life and sends none of them.
        _ => {}
    }
    Task::none()
}

/// Keys the focused text field does not want.
///
/// Enter is not here: `text_input` captures it and publishes `on_submit`
/// instead. Escape is read regardless of status, because the field captures
/// that too — to unfocus itself, which is not what a human pressing Escape at
/// a launcher is asking for.
pub fn subscription(_app: &App) -> Subscription<Message> {
    iced::event::listen_with(|event, _status, _window| match event {
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) => {
            use iced::keyboard::key::{Key, Named};
            match key {
                Key::Named(Named::Escape) => Some(Message::Close),
                Key::Named(Named::ArrowUp) => Some(Message::Move(-1)),
                Key::Named(Named::ArrowDown) => Some(Message::Move(1)),
                _ => None,
            }
        }
        // The compositor asked us to go — Super+C, or anything else that
        // closes a window. wlr-layer-shell's `closed` is not a request we may
        // decline, and `iced_layershell` only tears down its own bookkeeping
        // for it; leaving is our job.
        iced::Event::Window(iced::window::Event::Closed) => Some(Message::Close),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, terminal: bool) -> Entry {
        Entry {
            id: name.to_lowercase(),
            name: name.to_owned(),
            comment: None,
            argv: vec!["/bin/true".to_owned()],
            terminal,
            keywords: Vec::new(),
        }
    }

    fn app(entries: Vec<Entry>) -> App {
        let mut app = App {
            entries,
            query: String::new(),
            matched: Vec::new(),
            selected: 0,
            problem: None,
            term: None,
        };
        app.refilter();
        app
    }

    /// An empty field is not an empty launcher: it lists everything, so the
    /// human can arrow through what is installed without knowing a name.
    #[test]
    fn an_empty_query_lists_everything() {
        let app = app(vec![entry("Files", false), entry("Editor", false)]);
        assert_eq!(app.matched.len(), 2);
    }

    /// We have no terminal to run it in, so `launch` refuses. The refusal is
    /// the answer; the launcher stays up to show it.
    #[test]
    fn a_terminal_entry_leaves_a_refusal_on_screen() {
        let mut app = app(vec![entry("Htop", true)]);
        let _ = update(&mut app, Message::Activate);
        assert!(app.problem.is_some());
    }

    /// Arrowing through nothing must not panic.
    #[test]
    fn moving_through_an_empty_list_is_a_no_op() {
        let mut app = app(Vec::new());
        let _ = update(&mut app, Message::Move(1));
        let _ = update(&mut app, Message::Move(-1));
        assert_eq!(app.selected, 0);
        assert!(app.current().is_none());
    }

    /// A query that matches fewer rows than the selection index must not
    /// leave the highlight off the end of the list.
    #[test]
    fn a_narrowing_query_pulls_the_selection_back() {
        let mut app = app(vec![entry("Files", false), entry("Firefox", false)]);
        let _ = update(&mut app, Message::Select(1));
        let _ = update(&mut app, Message::Query("firef".to_owned()));
        assert!(app.selected < app.matched.len());
    }

    /// A refusal is about one row. Moving off that row takes the refusal
    /// with it, or it reads as a verdict on the row now selected.
    #[test]
    fn moving_off_a_refused_row_clears_the_refusal() {
        let mut app = app(vec![entry("Btop", true), entry("Files", false)]);
        let _ = update(&mut app, Message::Activate);
        assert!(app.problem.is_some());
        let _ = update(&mut app, Message::Move(1));
        assert!(app.problem.is_none());
    }

    /// An injected layer variant must be a no-op, not a panic.
    #[test]
    fn an_injected_layer_variant_is_a_no_op() {
        let mut app = app(Vec::new());
        let _ = update(&mut app, Message::SizeChange((crate::WIDTH, 100)));
        assert!(app.problem.is_none());
    }
}
