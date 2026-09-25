// SPDX-License-Identifier: AGPL-3.0-only
//! Step 3: network.
//!
//! Hero: the *schedule band*, a bordered statement of whether you are online,
//! with the medium and the connection at its right. Under it the access points
//! as a list, and, once one is chosen, a prompt band for its passphrase.
//! The accented value: the online banner. Offline it is the status red, never
//! the accent, and says what the install needs the network for; the step is
//! skippable but never hides that.

use super::{list_box, Body};
use crate::model::{ids, Load, Message, Model, Secret};
use crate::net::Link;
use crate::parts::{self, El, Tone};
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget::prompt_band;
use iced::widget::{container, row, text, text_input, Column, Space};
use iced::{Alignment, Length};

pub fn body(m: &Model) -> Body<'_> {
    let (banner, chip) = match &m.net {
        Load::Idle | Load::Loading => (
            parts::banner(Tone::Quiet, "network", "Looking for a connection", "Asking NetworkManager what is connected.", vec![]),
            ("checking".to_owned(), "network manager".to_owned()),
        ),
        Load::Failed(_) => (
            parts::banner(Tone::Quiet, "network", "Could not check", "The network state could not be read.", vec![]),
            ("unknown".to_owned(), "network manager".to_owned()),
        ),
        Load::Ready(s) => match &s.link {
            Link::Online { medium, name } => (
                parts::banner(
                    Tone::Accent,
                    "network",
                    "Online",
                    &format!("Connected to {name}. The install downloads its packages over this."),
                    vec![("online".to_owned(), medium.clone()), ("via".to_owned(), name.clone())],
                ),
                ("online".to_owned(), name.clone()),
            ),
            Link::Offline | Link::Unknown => (
                parts::banner(
                    Tone::Danger,
                    "network",
                    "Needs network",
                    "The install downloads its packages, so it cannot finish offline. Join a network below. You can skip this and connect later, but the install will stop at its first download.",
                    vec![("offline".to_owned(), "no connection".to_owned())],
                ),
                ("offline".to_owned(), "needs network".to_owned()),
            ),
            Link::NoManager => (
                parts::banner(
                    Tone::Quiet,
                    "network",
                    "No network manager",
                    "This session cannot join a network from here. A cable that is already connected still works.",
                    vec![("unmanaged".to_owned(), "no nmcli".to_owned())],
                ),
                ("unmanaged".to_owned(), "no network manager".to_owned()),
            ),
        },
    };

    let mut blocks: Vec<El<'_, Message>> = vec![banner];

    // --- access points
    let mut rows = Column::new();
    let mut count = 0;
    if let Load::Ready(s) = &m.net {
        for ap in &s.aps {
            count += 1;
            let mut r = row![
                parts::signal_bars(ap.signal),
                text(ap.ssid.clone())
                    .font(font::UI_MEDIUM)
                    .size(size::BODY)
                    .style(theme::text_primary),
                Space::new().width(Length::Fill),
            ]
            .spacing(space::CARD)
            .align_y(Alignment::Center);
            if ap.in_use {
                r = r.push(parts::mono("connected", color::TEXT_SECONDARY));
            }
            r = r.push(parts::mono(
                if ap.secure { "secured" } else { "open" },
                color::TEXT_TERTIARY,
            ));
            rows = rows.push(parts::pick(
                r,
                m.ap.as_deref() == Some(ap.ssid.as_str()),
                false,
                Some(Message::SelectAp(ap.ssid.clone())),
            ));
        }
    }
    if count == 0 {
        let note = match &m.net {
            Load::Ready(_) => "No wireless networks in range.",
            _ => "Scanning.",
        };
        rows = rows.push(container(parts::body(note)).padding([space::ROW_Y, space::CARD]));
    }
    let heading = if count == 0 {
        "Wireless".to_owned()
    } else {
        format!("Wireless, {count} in range")
    };
    blocks.push(list_box(
        &heading,
        vec![parts::verb("rescan", Message::Rescan)],
        None,
        rows,
    ));

    // --- join
    if let Some(ap) = m.ap_choice() {
        let status: Option<El<'_, Message>> = if m.joining {
            Some(parts::mono("joining", color::TEXT_SECONDARY))
        } else if m.join_failed {
            Some(parts::problem(
                "Could not join. Check the passphrase and try again.",
            ))
        } else {
            None
        };
        let join = parts::verb(if m.joining { "Joining" } else { "Join" }, Message::Join);
        let join: El<'_, Message> = if m.can_join() {
            join
        } else {
            parts::ghost("Join", None)
        };
        if ap.secure {
            let field = text_input(&format!("Passphrase for {}", ap.ssid), m.passphrase.as_str())
                .id(ids::PASSPHRASE)
                .secure(true)
                .on_input(|s| Message::Passphrase(Secret::new(s)))
                .on_submit(Message::Join)
                .font(font::DATA)
                .size(size::PROMPT)
                .padding(0)
                .style(theme::prompt_input);
            let mut trailing = row![].spacing(space::CARD).align_y(Alignment::Center);
            if let Some(s) = status {
                trailing = trailing.push(s);
            }
            trailing = trailing.push(join);
            blocks.push(prompt_band("key", field, Some(trailing.into())));
        } else {
            let mut r = row![
                parts::body(format!(
                    "{} is open: anyone nearby can read what is sent over it.",
                    ap.ssid
                )),
                Space::new().width(Length::Fill),
            ]
            .spacing(space::CARD)
            .align_y(Alignment::Center);
            if let Some(s) = status {
                r = r.push(s);
            }
            r = r.push(join);
            blocks.push(container(r).padding([space::ROW_Y, space::CARD]).into());
        }
    } else if m.join_failed {
        blocks.push(parts::problem("Could not join."));
    }

    Body {
        subtitle: "The install needs a connection to download the system. Skip it if you are wired or will connect later.".to_owned(),
        chip,
        blocks,
        scroll: false,
    }
}
