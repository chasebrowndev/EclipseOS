// SPDX-License-Identifier: AGPL-3.0-only
//! Cheat-sheet state.
//!
//! The sheet is read once, when it opens: the binds it lists are the binds
//! the compositor is holding at that moment, and a reload while it is open is
//! not worth a subscription. It goes away on Escape, or when the bind opens
//! the next one.

use iced::{Subscription, Task};
use iced_layershell::to_layer_message;

use ec_abyss_config::{defaults_with_launcher, mode_default_binds, Config};

use crate::model::{self, Entry};

#[to_layer_message]
#[derive(Debug, Clone)]
pub enum Message {
    /// The compositor's theme moved and the palette was re-applied (ADR 0084);
    /// nothing to do but let the view rebuild.
    ThemeChanged,
    /// Escape, or the bind pressed again.
    Close,
    /// A chord row was pressed. Rows answer the pointer and do nothing else.
    Ignore,
}

pub struct App {
    /// Every bind the live config holds that has a line on the sheet.
    pub entries: Vec<Entry>,
    /// Chords on the sheet that differ from the stock table.
    pub changed: usize,
    /// The mode the config runs in, as the status chip names it.
    pub mode: String,
    /// The panel's glass radius and whether blur is behind it, read once at
    /// startup. Fail-soft: the compile-time token and an opaque sheet.
    pub glass_radius: f32,
    pub blur: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        // `load` merges the file over the stock table, so `binds` is what the
        // compositor holds. The baseline is the same table it was merged over.
        let config = Config::load(None);
        let mut baseline = defaults_with_launcher(&config.launcher);
        baseline.extend(mode_default_binds(config.mode));
        let entries = model::entries(&config.binds, &baseline);
        let changed = entries.iter().filter(|e| e.custom).count();
        let (radius, blur) = ec_ui::ipc::fetch_glass();
        App {
            entries,
            changed,
            mode: format!("{:?}", config.mode).to_lowercase(),
            glass_radius: radius.unwrap_or(ec_ui::tokens::radius::CARD),
            blur: blur.unwrap_or(false),
        }
    }
}

pub fn update(_app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::ThemeChanged | Message::Ignore => {}
        Message::Close => return iced::exit(),
        // `to_layer_message` injects layer-control variants; the sheet has a
        // fixed size for its whole short life and sends none of them.
        _ => {}
    }
    Task::none()
}

/// Escape closes the sheet. The compositor delivers the keys: the sheet holds
/// the keyboard while it is open, so a click is not needed first.
pub fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::batch([
        iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) => {
                use iced::keyboard::{key::Named, Key};
                match key {
                    Key::Named(Named::Escape) => Some(Message::Close),
                    _ => None,
                }
            }
            _ => None,
        }),
        ec_ui::ipc::theme_subscription(|| Message::ThemeChanged),
    ])
}
