// SPDX-License-Identifier: AGPL-3.0-only
//! State and update. There is one action: re-read the files.

use iced::{Element, Task, Theme};

use crate::read::{self, Policy};
use crate::view;

#[derive(Debug, Clone)]
pub enum Message {
    /// Re-read every file on the search path.
    Reload,
}

pub struct App {
    pub policy: Policy,
    /// The panel's glass radius, read once from `decoration.rounding` at
    /// startup (BLUR-06). `crate::conn::fetch_glass_radius` is fail-soft, so
    /// this falls back to the compile-time token when nothing answers.
    pub glass_radius: f32,
}

impl App {
    pub fn new() -> Self {
        Self {
            policy: read::load(),
            glass_radius: crate::conn::fetch_glass_radius().unwrap_or(eclipse_ui::tokens::radius::CARD),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Reload => app.policy = read::load(),
    }
    Task::none()
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    view::view(&app.policy, app.glass_radius)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_replaces_the_whole_read_rather_than_merging_into_it() {
        let mut app = App {
            policy: Policy::default(),
            glass_radius: eclipse_ui::tokens::radius::CARD,
        };
        let _ = update(&mut app, Message::Reload);
        // One report per file on the search path, whether or not it exists.
        assert_eq!(app.policy.files.len(), read::search_path().len());
    }

    #[test]
    fn a_machine_with_no_policy_file_still_renders() {
        let app = App {
            policy: Policy::default(),
            glass_radius: eclipse_ui::tokens::radius::CARD,
        };
        let _ = view(&app);
    }
}
