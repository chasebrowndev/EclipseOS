// SPDX-License-Identifier: AGPL-3.0-only
//! Step 5: identity.
//!
//! Hero: the form itself, three plain fields with their names above them. The
//! host name has a good default, so it sits under "More options".
//! No accented value: nothing here is a live reading. The password fields are
//! password-role: masked, never shown, never echoed in a problem sentence.

use super::Body;
use crate::model::{ids, Key, Message, Model, Secret};
use crate::parts::{self, El};
use eclipse_ui::tokens::space;
use iced::widget::Column;

pub fn body(m: &Model) -> Body<'_> {
    let next_field = Message::Key(Key::Tab);
    let user = parts::labeled(
        "Your name",
        parts::input(
            ids::USERNAME,
            "lowercase letters, starting with a letter",
            &m.username,
            false,
            !m.username.is_empty() && !m.username_ok(),
            Message::Username,
            next_field.clone(),
        ),
    );
    let pass = parts::labeled(
        "Password",
        parts::input(
            ids::PASSWORD,
            "at least 8 characters",
            m.password.as_str(),
            true,
            false,
            |s| Message::Password(Secret::new(s)),
            next_field.clone(),
        ),
    );
    let mismatch = !m.password2.is_empty() && !m.passwords_match();
    let again = parts::labeled(
        "Password again",
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

    let mut form = Column::new()
        .spacing(space::CARD)
        .push(user)
        .push(pass)
        .push(again);

    if m.more {
        form = form.push(parts::labeled(
            "Computer name",
            parts::input(
                ids::HOSTNAME,
                "eclipse",
                &m.hostname,
                false,
                !m.hostname.is_empty() && !m.hostname_ok(),
                Message::Hostname,
                Message::Key(Key::Tab),
            ),
        ));
        form = form.push(parts::link("Fewer options", Message::More(false)));
    } else {
        form = form.push(parts::link("More options", Message::More(true)));
    }

    // One sentence, and only once there is something to be wrong about.
    let problem = match m.identity_problem() {
        Some(p) if !(m.username.is_empty() && m.password.is_empty()) => Some(p),
        _ => None,
    };
    let mut blocks: Vec<El<'_, Message>> = vec![form.into()];
    if let Some(p) = problem {
        blocks.push(parts::problem(p));
    }

    Body {
        title: "Who are you?".to_owned(),
        lead: "This is the account you will sign in with.".to_owned(),
        blocks,
        scroll: true,
    }
}
