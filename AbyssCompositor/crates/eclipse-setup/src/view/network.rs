// SPDX-License-Identifier: AGPL-3.0-only
//! Step 3: network.
//!
//! Online, the whole step is one word, "Connected", and Continue; the wireless
//! panel appears only if the user asks for it ("Connect to Wi-Fi"). Not online,
//! the panel is the step: the networks in range, pick one, type its passphrase,
//! Join. The step is skippable but never hides that the install downloads its
//! packages.
//! The accented value: the selected network row (offline), or "Connected".

use super::{list_box, Body};
use crate::model::{ids, Load, Message, Model, Secret};
use crate::net::Link;
use crate::parts::{self, El};
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget::prompt_band;
use iced::widget::{column, container, row, text, text_input, Column, Space};
use iced::{Alignment, Length};

pub fn body(m: &Model) -> Body<'_> {
    let title = "Network".to_owned();
    match &m.net {
        Load::Idle | Load::Loading => Body {
            title,
            lead: "Checking the connection.".to_owned(),
            blocks: vec![],
            scroll: false,
        },
        Load::Failed(_) => Body {
            title,
            lead: "The network state could not be read. You can continue without it.".to_owned(),
            blocks: vec![],
            scroll: false,
        },
        Load::Ready(s) => match &s.link {
            Link::Online { .. } => {
                let mut blocks: Vec<El<'_, Message>> = Vec::new();
                if m.more {
                    blocks.push(wifi(m));
                } else {
                    blocks.push(parts::link("Connect to Wi-Fi", Message::More(true)));
                }
                Body {
                    title,
                    lead: "Connected".to_owned(),
                    blocks,
                    scroll: false,
                }
            }
            Link::NoManager => Body {
                title,
                lead: "This session cannot join a network from here. A cable that is already connected still works."
                    .to_owned(),
                blocks: vec![],
                scroll: false,
            },
            Link::Offline | Link::Unknown => Body {
                title: "Connect to the internet".to_owned(),
                lead: "The install downloads its packages. You can skip this, but it will stop at its first download."
                    .to_owned(),
                blocks: vec![wifi(m)],
                scroll: false,
            },
        },
    }
}

/// The networks in range and, once one is chosen, how to join it.
fn wifi(m: &Model) -> El<'_, Message> {
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
            } else if !ap.secure {
                r = r.push(parts::mono("open", color::TEXT_TERTIARY));
            }
            rows = rows.push(parts::pick(
                r,
                m.ap.as_deref() == Some(ap.ssid.as_str()),
                true,
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

    let mut col = Column::new().spacing(space::BLOCK).height(Length::Fill);
    col = col.push(list_box(None, rows));

    if let Some(ap) = m.ap_choice() {
        let status: Option<El<'_, Message>> = if m.joining {
            Some(parts::mono("joining", color::TEXT_SECONDARY))
        } else if m.join_failed {
            Some(parts::problem("Could not join. Check the passphrase."))
        } else {
            None
        };
        let join: El<'_, Message> = parts::primary(
            if m.joining { "Joining" } else { "Join" },
            m.can_join().then_some(Message::Join),
        );
        if ap.secure {
            let field = text_input("Passphrase", m.passphrase.as_str())
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
            col = col.push(prompt_band("key", field, Some(trailing.into())));
        } else {
            let mut r = row![
                parts::body("This network is open: anyone nearby can read what is sent over it."),
                Space::new().width(Length::Fill),
            ]
            .spacing(space::CARD)
            .align_y(Alignment::Center);
            if let Some(s) = status {
                r = r.push(s);
            }
            r = r.push(join);
            col = col.push(r);
        }
    } else {
        let mut r = row![parts::link("Rescan", Message::Rescan)].align_y(Alignment::Center);
        if m.join_failed {
            r = r
                .push(Space::new().width(space::CARD))
                .push(parts::problem("Could not join."));
        }
        col = col.push(column![r]);
    }
    col.into()
}
