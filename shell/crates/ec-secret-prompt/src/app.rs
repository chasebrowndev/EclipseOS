// SPDX-License-Identifier: AGPL-3.0-only
//! State, messages and view.
//!
//! Shape: a heading (what the secret is for), the hero — a prompt band holding
//! the one field, or in the yes/no and display modes the code itself — and an
//! action row. Three silhouettes: bare text, a glass band, a row of pills.
//!
//! Accent ledger: the one yellow is the committing pill ("Join" / "Pair" / "Save").
//! A `Show` window commits nothing — its pill only dismisses — so there the
//! yellow moves to the code the human has to type. The band is neutral, the
//! caret is furniture, and a refusal is `DANGER`, which is not the accent.

use std::fmt;

use iced::widget::{column, container, row, text, text_input, Space};
use iced::{Alignment, Element, Length, Subscription, Task, Theme};

use ec_ui::theme;
use ec_ui::tokens::{font, secret, size, space};
use ec_ui::widget::{micro_label, pill, prompt_band};

use crate::{service, Buffer, Target};

const FIELD: &str = "secret";
/// The band's mono marker. A caret, not a word: at prompt scale a word
/// reads as placeholder text.
const MARKER: &str = "›";
/// The marker before a code the window shows rather than takes.
const CODE_MARKER: &str = "#";
/// The marker before a yes/no question with no code to compare.
const ASK_MARKER: &str = "?";
/// The field's placeholder once its value is with the service.
const SENT: &str = "sent";
/// The field's placeholder once the service has kept the value.
const STORED: &str = "stored";
/// How long a stored API key's window says so before it closes: long
/// enough to read one word, short enough not to be a second dialog.
const SAVED_FOR: std::time::Duration = std::time::Duration::from_millis(900);

#[derive(Clone)]
pub enum Message {
    /// The field's whole new value. Never `Debug`-printed: see the impl.
    Typed(String),
    Submit,
    /// The service's verdict on the one attempt in flight.
    Done(Result<(), String>),
    Cancel,
    /// The success note has been read; the window goes.
    Close,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::Typed(_) => f.write_str("Typed(<redacted>)"),
            Message::Submit => f.write_str("Submit"),
            Message::Done(r) => write!(f, "Done({r:?})"),
            Message::Cancel => f.write_str("Cancel"),
            Message::Close => f.write_str("Close"),
        }
    }
}

pub struct App {
    target: Target,
    typed: Buffer,
    /// The last refusal, in words. Never contains the secret.
    problem: Option<String>,
    /// An attempt is with the service. A second Submit waits for its answer
    /// rather than racing it.
    pending: bool,
    /// The service took it. Only an API key window lingers on this (the
    /// key has no other visible effect); the rest close at once.
    saved: bool,
}

impl App {
    pub fn new(target: Target) -> (Self, Task<Message>) {
        #[allow(unused_mut)]
        let mut app = Self {
            target,
            typed: Buffer::default(),
            problem: None,
            pending: false,
            saved: false,
        };
        // Debug-build fixture: open already waiting on the service, so the
        // pending state can be looked at without sending anything anywhere.
        #[cfg(debug_assertions)]
        match std::env::var("SECRET_PROMPT_PREVIEW").as_deref() {
            Ok("pending") => app.pending = true,
            Ok("saved") => app.saved = true,
            Ok("refused") => app.problem = Some("The secret store is locked. Unlock it first.".to_owned()),
            _ => {}
        }
        // The window exists for one keystroke sequence; it opens ready for it.
        #[allow(unused_mut)]
        let mut task = iced::widget::operation::focus(FIELD);
        #[cfg(debug_assertions)]
        if let Ok(path) = std::env::var("SECRET_PROMPT_SHOT") {
            task = Task::batch([task, preview_shot(path)]);
        }
        (app, task)
    }

    pub fn title(&self) -> &'static str {
        self.target.title()
    }
}

/// Debug builds only: read the window's frame back after a moment, write it
/// to `path` as `WxH\n` and raw RGBA, and exit — the screenshot loop on a
/// host with no screencopy client. The field is empty in every fixture, so
/// nothing secret can land in the file.
#[cfg(debug_assertions)]
fn preview_shot(path: String) -> Task<Message> {
    let (tx, rx) = iced::futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(3000));
        let _ = tx.send(());
    });
    Task::perform(
        async move {
            let _ = rx.await;
        },
        |()| (),
    )
    .then(|()| iced::window::latest())
    .and_then(iced::window::screenshot)
    .then(move |shot| {
        let head = format!("{}x{}\n", shot.size.width, shot.size.height);
        let _ = std::fs::write(&path, [head.as_bytes(), &shot.rgba[..]].concat());
        iced::exit()
    })
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Typed(new) => {
            if app.target.admits(&new) {
                app.typed.replace(new);
                app.problem = None;
            } else {
                crate::wipe(new);
            }
        }
        Message::Submit => {
            if app.pending || !app.target.ready(app.typed.as_str()) {
                return Task::none();
            }
            // A display-only window has nothing to send: "Done" just closes.
            if !app.target.answers() {
                return iced::exit();
            }
            // A refused secret is not kept for a second try: the human
            // retypes it, and we hold it for no longer than the one attempt.
            // The join can take tens of seconds, so it runs off this thread.
            app.pending = true;
            let secret = app.typed.take();
            let target = app.target.clone();
            let (tx, rx) = iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(service::submit(&target, secret));
            });
            return Task::perform(
                async move {
                    rx.await
                        .unwrap_or_else(|_| Err("The attempt was lost.".to_owned()))
                },
                Message::Done,
            );
        }
        Message::Done(Ok(())) if matches!(app.target, Target::ApiKey { .. }) => {
            app.pending = false;
            app.saved = true;
            let (tx, rx) = iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                std::thread::sleep(SAVED_FOR);
                let _ = tx.send(());
            });
            return Task::perform(async move { rx.await.ok() }, |_| Message::Close);
        }
        Message::Done(Ok(())) => return iced::exit(),
        Message::Close => return iced::exit(),
        Message::Done(Err(problem)) => {
            app.pending = false;
            app.problem = Some(problem);
            return iced::widget::operation::focus(FIELD);
        }
        // Saved, and on its way out: nothing left to refuse.
        Message::Cancel if app.saved => return iced::exit(),
        Message::Cancel => {
            app.typed.clear();
            // No-op for a `Show` window: there is no request to refuse.
            service::reject(&app.target);
            return iced::exit();
        }
    }
    Task::none()
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    let t = &app.target;

    let heading = column![
        micro_label(t.kind()),
        text(t.heading())
            .font(font::UI_MEDIUM)
            .size(size::CARD_TITLE)
            .style(theme::text_primary),
    ]
    .spacing(secret::TITLE_GAP);

    let band = if let Some(code) = t.shown() {
        // The number to compare, or the code to type: read, not edited, so it
        // stands at magnitude scale where the field would be.
        let ink = if t.answers() {
            theme::text_primary
        } else {
            theme::text_accent
        };
        prompt_band(
            CODE_MARKER,
            text(code)
                .font(font::DATA_MEDIUM)
                .size(size::BIG_NUMBER)
                .style(ink),
            None,
        )
    } else if t.takes_input() {
        // While the attempt is out the field takes nothing: a keystroke now
        // would be typed into a value that is already gone. The value has
        // been taken, so the placeholder says where it went rather than
        // inviting a retype.
        let hint = match (app.saved, app.pending) {
            (true, _) => STORED,
            (false, true) => SENT,
            (false, false) => t.hint(),
        };
        let mut field = text_input(hint, app.typed.as_str())
            .id(FIELD)
            .secure(true)
            .font(font::DATA)
            .size(size::BODY)
            .style(theme::prompt_input);
        if !app.pending && !app.saved {
            field = field.on_input(Message::Typed).on_submit(Message::Submit);
        }
        prompt_band(MARKER, field, None)
    } else {
        prompt_band(
            ASK_MARKER,
            text("Pair only if you started this.")
                .font(font::DATA)
                .size(size::BODY)
                .style(theme::text_primary),
            None,
        )
    };

    // Left of the pills: the refusal if there is one, the wait while the
    // service has the attempt, otherwise the promise.
    let note: Element<'_, Message, Theme> = match (&app.problem, app.pending) {
        _ if app.saved => text("Saved.")
            .size(size::BODY_SMALL)
            .style(theme::text_secondary)
            .into(),
        (Some(p), _) => text(p.clone())
            .size(size::BODY_SMALL)
            .style(theme::text_danger)
            .into(),
        (None, true) => text("Waiting for the service…")
            .size(size::BODY_SMALL)
            .style(theme::text_secondary)
            .into(),
        (None, false) => text(t.promise())
            .size(size::BODY_SMALL)
            .style(theme::text_tertiary)
            .into(),
    };

    let verb = if app.pending { t.working() } else { t.verb() };
    let mut actions = row![container(note).width(Length::Fill)]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    if app.saved {
        // Done is the only thing left to say; the window is already going.
        actions = actions.push(pill("Done", false, Message::Close));
    } else if t.answers() {
        actions =
            actions
                .push(pill("Cancel", false, Message::Cancel))
                .push(pill(verb, true, Message::Submit));
    } else {
        actions = actions.push(pill(verb, false, Message::Submit));
    }

    container(column![heading, band, Space::new().height(Length::Fill), actions].spacing(space::BLOCK))
        .padding([space::PANE_Y, space::PANE_X])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window(ec_ui::tokens::radius::WINDOW))
        .into()
}

/// Escape cancels. The field captures Escape to unfocus itself, so it is read
/// regardless of capture status; nothing else about a key is looked at, and
/// nothing about one is logged.
pub fn subscription(_app: &App) -> Subscription<Message> {
    iced::event::listen_with(|event, _status, _window| match event {
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            ..
        }) => Some(Message::Cancel),
        _ => None,
    })
}
