// SPDX-License-Identifier: AGPL-3.0-only
//! The lock screen (ADR 0095). Owned by eclipse-frontend.
//!
//! The greeter's family (packaging/greetd/regreet.css): one large clock, then
//! one glass card on a warm near-black ground. The surface is opaque, so the
//! card's depth is its fill, rim and drop inside it, never translucency.
//!
//! ```text
//!                 9:41 AM              the hero: the time, the one numeral
//!           Wednesday, November 11
//!   ╭──────────────────────────────────╮
//!   │               (R)                │  account: disc over its name
//!   │              robin               │
//!   │             L O C K E D          │
//!   │ ╭──────────────────────────────╮ │
//!   │ │ ›  ● ● ● ● ● ●|      (Unlock)│ │  the field: dots only, no text
//!   │ ╰──────────────────────────────╯ │
//!   │ Wrong password     retry in 4 s  │  the message line
//!   ╰──────────────────────────────────╯
//!            [enter] unlock  [esc] clear
//! ```
//!
//! Three silhouettes, none repeated: bare type, a card, a hint line.
//!
//! **Accent ledger.** The one yellow is the typed length: the dot run and its
//! caret, while the field takes keys. Unlock is a white lift, not a gold
//! button; a failure is `danger`, not accent.
//!
//! **Every output** draws the same scene. Keys reach the model from whichever
//! surface has the keyboard, and the view cannot tell which output the human
//! is facing, so every one shows the field and its feedback.
//!
//! Motion: the view has no frame clock (the model ticks once a second), so
//! nothing here animates; every change is a cut between still states, which
//! also satisfies reduce-motion.
//!
//! The contract (see `app.rs`): read only `App::{status, typed, user, clock,
//! can_submit}`; emit only `Message::Submit` / `Message::Clear`; no
//! `text_input`; never a window-close task.

use iced::widget::{button, column, container, row, text, Space};
use iced::{Alignment, Element, Length, Shadow, Theme, Vector};

use ec_ui::theme;
use ec_ui::tokens::{color, font, lock, size, space};
use ec_ui::widget::{avatar, dot_run, key_hints, micro_label, secret_field, spinner, FieldState};

use crate::app::{App, Message, Status};
use crate::clock::Reading;
use crate::machine::Reason;

/// The field's marker, as in the secret prompt.
const MARKER: &str = "\u{203a}";

/// Everything the view draws, read once from the model. `pub` so the preview
/// example can draw any state without a lock, a PAM stack or a grant.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    pub status: Status,
    pub typed: usize,
    pub user: &'a str,
    pub clock: &'a Reading,
    pub can_submit: bool,
}

impl<'a> Scene<'a> {
    pub fn of(app: &'a App) -> Self {
        Scene {
            status: app.status(),
            typed: app.typed(),
            user: app.user(),
            clock: app.clock(),
            can_submit: app.can_submit(),
        }
    }

    /// Whether keys land in the field (the machine's `accepts_input`).
    fn taking(&self) -> bool {
        matches!(
            self.status,
            Status::Locking | Status::Ready | Status::Failed { .. }
        )
    }
}

pub fn view(app: &App, _window: iced::window::Id) -> Element<'_, Message, Theme> {
    scene(Scene::of(app))
}

/// The lock screen for one output. Every output gets the same one.
pub fn scene(s: Scene<'_>) -> Element<'_, Message, Theme> {
    let clock = column![
        text(s.clock.time.as_str())
            .font(font::UI)
            .size(lock::CLOCK)
            .style(theme::text_primary),
        text(s.clock.date.as_str())
            .font(font::UI)
            .size(size::PROMPT)
            .style(theme::text_secondary),
    ]
    .spacing(lock::DATE_GAP)
    .align_x(Alignment::Center);

    // The account sits centred on top of the card, a disc over its name, the
    // way a phone or a laptop greets: the card holds one person and one field.
    let account = column![
        avatar(s.user, lock::AVATAR),
        column![
            text(s.user)
                .font(font::UI_MEDIUM)
                .size(size::PROMPT)
                .style(theme::text_primary)
                .wrapping(text::Wrapping::None),
            micro_label(if s.status == Status::Unlocking {
                "unlocked"
            } else {
                "locked"
            }),
        ]
        .spacing(space::LINE_GAP)
        .align_x(Alignment::Center),
    ]
    .spacing(space::CONTROL_GAP)
    .align_x(Alignment::Center)
    .width(Length::Fill);

    let card = container(column![account, field(&s), note(&s)].spacing(space::CARD))
        .padding(lock::CARD_PAD)
        .width(Length::Fixed(lock::CARD_W))
        .style(card_ground);

    // Nothing to press while PAM holds the password or the lock is going;
    // the line keeps its height so the card does not move.
    let hints: Element<'_, Message, Theme> = if s.taking() {
        key_hints(&[("enter", "unlock"), ("esc", "clear")])
    } else {
        Space::new().height(space::KEYCAP_H).into()
    };

    container(
        column![clock, Space::new().height(lock::CLOCK_GAP), card, hints]
            .spacing(space::BLOCK)
            .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

/// The field: the typed length while it takes keys, the check while PAM has
/// the password, the exit while the lock goes.
fn field<'a>(s: &Scene<'a>) -> Element<'a, Message, Theme> {
    let quiet = |label: &str| {
        text(label.to_owned())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_secondary)
    };
    let (state, body, trailing): (FieldState, Element<'a, Message, Theme>, _) = match s.status {
        Status::Checking => (
            FieldState::Resting,
            row![
                spinner(lock::SPIN_PHASE, color::text_secondary()),
                quiet("Checking\u{2026}")
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center)
            .into(),
            None,
        ),
        Status::Unlocking => (FieldState::Resting, quiet("Unlocking\u{2026}").into(), None),
        Status::Failed { .. } | Status::Locking | Status::Ready => {
            let state = if matches!(s.status, Status::Failed { .. }) {
                FieldState::Refused
            } else {
                FieldState::Taking
            };
            let body: Element<'a, Message, Theme> = if s.typed == 0 {
                row![
                    dot_run(0, color::accent()),
                    text("Password")
                        .font(font::UI)
                        .size(size::BODY)
                        .style(theme::text_tertiary),
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center)
                .into()
            } else {
                dot_run(s.typed, color::accent())
            };
            (state, body, Some(unlock(s.can_submit)))
        }
    };
    secret_field(state, MARKER, body, trailing)
        .height(Length::Fixed(lock::FIELD_H))
        .into()
}

/// Unlock: a ghost until there is something PAM may check, then a white lift.
fn unlock<'a>(ready: bool) -> Element<'a, Message, Theme> {
    button(text("Unlock").font(font::UI_MEDIUM).size(size::BODY_SMALL))
        .padding([space::PILL_Y, space::PILL_X])
        .style(theme::send(ready))
        .on_press_maybe(ready.then_some(Message::Submit))
        .into()
}

/// One line under the field: what is happening at the left, the machine's
/// reading of it (the wait, the count) in mono at the right.
fn note<'a>(s: &Scene<'a>) -> Element<'a, Message, Theme> {
    let (line, tone, reading): (&str, fn(&Theme) -> text::Style, Option<String>) = match s.status {
        Status::Locking | Status::Ready => ("Enter your password to unlock", theme::text_tertiary, None),
        Status::Checking => ("Checking your password", theme::text_secondary, None),
        Status::Unlocking => ("Welcome back", theme::text_secondary, None),
        Status::Failed {
            reason,
            failures,
            retry_in_secs,
        } => {
            let line = match reason {
                Reason::Wrong => "Wrong password",
                Reason::Unavailable => "Authentication is unavailable",
            };
            let reading = match (retry_in_secs, reason) {
                (Some(secs), _) => format!("retry in {secs} s"),
                (None, Reason::Unavailable) => "try again".to_owned(),
                (None, Reason::Wrong) if failures == 1 => "1 failed try".to_owned(),
                (None, Reason::Wrong) => format!("{failures} failed tries"),
            };
            (line, theme::text_danger, Some(reading))
        }
    };
    let mut r = row![container(
        text(line)
            .font(font::UI)
            .size(size::BODY_SMALL)
            .style(tone)
            .wrapping(text::Wrapping::None)
    )
    .width(Length::Fill)]
    .align_y(Alignment::Center);
    if let Some(reading) = reading {
        r = r.push(
            text(reading)
                .font(font::DATA)
                .size(size::MONO)
                .style(theme::text_tertiary)
                .wrapping(text::Wrapping::None),
        );
    }
    container(r).height(Length::Fixed(lock::NOTE_H)).into()
}

/// The card: the glass panel's fill and rim, resting on the ground with a
/// soft drop. The ground is opaque, so the drop is depth, not a halo.
fn card_ground(t: &Theme) -> container::Style {
    let mut s = theme::panel(lock::CARD_RADIUS)(t);
    s.border.color = color::border_strong();
    // Over an opaque ground the panel's white tint reads as grey paint; the
    // card is the next warm surface up instead, with the panel's rim.
    s.background = Some(iced::Background::Color(color::surface_1()));
    s.shadow = Shadow {
        color: color::cell_shadow(),
        offset: Vector::new(space::ZERO, lock::CARD_SHADOW_Y),
        blur_radius: lock::CARD_SHADOW_BLUR,
    };
    s
}

/// The surface ground: opaque, the theme's base. A lock surface that let the
/// desktop through would show what it locks.
pub fn style(_app: &App, theme: &Theme) -> iced::theme::Style {
    ground(theme)
}

/// [`style`] without the model, for the preview.
pub fn ground(theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: theme.palette().background,
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ground_is_opaque() {
        let t = theme::theme();
        assert_eq!(ground(&t).background_color.a, 1.0);
    }
}
