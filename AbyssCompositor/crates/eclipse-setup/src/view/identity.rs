// SPDX-License-Identifier: AGPL-3.0-only
//! Step 5: identity.
//!
//! Hero: the handle. `user@host` set large in a band, so the form under it has
//! a result you can watch take shape instead of four boxes to fill.
//! The accented value: the handle, once both halves are valid. Until then it is
//! the quiet ink, so the pane is not yellow about something that is not true.
//! The password fields are password-role: masked, never shown, never echoed in
//! a problem sentence.

use super::Body;
use crate::model::{ids, Key, Message, Model, Secret};
use crate::parts::{self, El};
use eclipse_setup_plan::MIN_PASSWORD_CHARS;
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, radius, size, space};
use eclipse_ui::widget::{hairline, panel, prompt_band};
use iced::widget::{column, container, text};
use iced::Length;

pub fn body(m: &Model) -> Body<'_> {
    let ready_handle = m.username_ok() && m.hostname_ok();
    let handle = format!(
        "{}@{}",
        if m.username.is_empty() {
            "user"
        } else {
            m.username.as_str()
        },
        if m.hostname.is_empty() {
            "host"
        } else {
            m.hostname.as_str()
        },
    );
    let handle_text = text(handle.clone())
        .font(font::DATA_MEDIUM)
        .size(size::BIG_NUMBER)
        .wrapping(text::Wrapping::None);
    let handle_text = if ready_handle {
        handle_text.style(theme::text_accent)
    } else {
        handle_text.style(theme::text_tertiary)
    };
    let hero = prompt_band("$", handle_text, None);

    let next_field = Message::Key(Key::Tab);
    let host = parts::field_row(
        "Host name",
        "how the machine is called on a network",
        parts::input(
            ids::HOSTNAME,
            "eclipse",
            &m.hostname,
            false,
            !m.hostname.is_empty() && !m.hostname_ok(),
            Message::Hostname,
            next_field.clone(),
        ),
    );
    let user = parts::field_row(
        "User name",
        "lowercase, starts with a letter",
        parts::input(
            ids::USERNAME,
            "you",
            &m.username,
            false,
            !m.username.is_empty() && !m.username_ok(),
            Message::Username,
            next_field.clone(),
        ),
    );
    let pass = parts::field_row(
        "Password",
        "at least 8 characters, for signing in and sudo",
        parts::input(
            ids::PASSWORD,
            "",
            m.password.as_str(),
            true,
            false,
            |s| Message::Password(Secret::new(s)),
            next_field,
        ),
    );
    let mismatch = !m.password2.is_empty() && !m.passwords_match();
    let again = parts::field_row(
        "Repeat password",
        "the same again",
        parts::input(
            ids::PASSWORD2,
            "",
            m.password2.as_str(),
            true,
            mismatch,
            |s| Message::Password2(Secret::new(s)),
            Message::Key(Key::Enter),
        ),
    );
    let form = panel(
        radius::CARD,
        column![host, hairline(), user, hairline(), pass, hairline(), again],
    )
    .padding(0);

    let typed = m.password_chars();
    let short = typed > 0 && typed < MIN_PASSWORD_CHARS;
    let count = parts::mono(
        format!("{typed} characters, at least {MIN_PASSWORD_CHARS} needed"),
        if short {
            color::DANGER
        } else {
            color::TEXT_TERTIARY
        },
    );
    let note: El<'_, Message> = match m.identity_problem() {
        Some(p) if !(m.username.is_empty() && m.password.is_empty()) => {
            column![parts::problem(p), count].spacing(space::CHIP_GAP).into()
        }
        _ => column![
            container(parts::body(
                "The password is handed to the installer once, and cleared here the moment it is sent.",
            ))
            .width(Length::Fill),
            count,
        ]
        .spacing(space::CHIP_GAP)
        .into(),
    };

    let chip = (
        handle,
        if m.password.is_empty() {
            "no password yet"
        } else {
            "password set"
        }
        .to_owned(),
    );
    Body {
        subtitle: "Who owns the machine.".to_owned(),
        chip,
        blocks: vec![hero, form.into(), note],
        scroll: true,
    }
}
