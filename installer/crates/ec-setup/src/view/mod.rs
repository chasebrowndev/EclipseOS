// SPDX-License-Identifier: AGPL-3.0-only
//! The wizard's screens.
//!
//! One decision per screen: a progress line, a big plain title, one sentence,
//! the step's own block and a footer with Back and the one forward action. There
//! is no sidebar and no key-hint strip; the keyboard still works and is not
//! advertised. Anything advanced sits behind a small "More options" link. The
//! step files decide what the hero is and where the pane's one yellow goes
//! (docs/COMPOSITION.md); each says so in its own header.

mod appearance;
mod apps;
mod components;
mod disk;
mod identity;
mod install;
mod keyboard;
mod language;
mod layout;
mod mode;
mod network;
mod profile;
mod review;
mod timezone;

use crate::metrics;
use crate::model::{Message, Model, Phase, Step};
use crate::parts::{self, El};
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, space};
use eclipse_ui::widget::hairline;
use iced::widget::{column, container, row, scrollable, Column, Space};
use iced::{Alignment, Length};

/// What a step contributes to the shell.
pub struct Body<'a> {
    pub title: String,
    /// One plain sentence; empty for none.
    pub lead: String,
    pub blocks: Vec<El<'a, Message>>,
    /// The blocks scroll as one column. Steps whose lists fill the height
    /// themselves leave this off.
    pub scroll: bool,
}

/// The whole window for steps 1 to 14. Step 0 is `eclipse_welcome`'s own view.
pub fn view(m: &Model) -> El<'_, Message> {
    let body = match m.step {
        Step::Welcome => {
            return container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        Step::Language => language::body(m),
        Step::Keyboard => keyboard::body(m),
        Step::Timezone => timezone::body(m),
        Step::Network => network::body(m),
        Step::Disk => disk::body(m),
        Step::Identity => identity::body(m),
        Step::Profile => profile::body(m),
        Step::Mode => mode::body(m),
        Step::Layout => layout::body(m),
        Step::Components => components::body(m),
        Step::Appearance => appearance::body(m),
        Step::Apps => apps::body(m),
        Step::Review => review::body(m),
        Step::Install => install::body(m),
    };

    let mut head = Column::new()
        .spacing(space::CHIP_GAP)
        .push(parts::title(&body.title));
    if !body.lead.is_empty() {
        head = head.push(parts::lead(&body.lead));
    }

    let mut blocks = Column::new()
        .spacing(space::BLOCK)
        .width(Length::Fill)
        .height(if body.scroll { Length::Shrink } else { Length::Fill });
    for b in body.blocks {
        blocks = blocks.push(b);
    }
    let blocks: El<'_, Message> = if body.scroll {
        scrollable(container(blocks).padding(iced::Padding {
            right: space::CARD,
            ..iced::Padding::ZERO
        }))
        .style(theme::eclipse_scrollable)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    } else {
        container(blocks).width(Length::Fill).height(Length::Fill).into()
    };

    let mut content = Column::new().spacing(space::BLOCK).height(Length::Fill);
    if let Some(here) = m.step.position() {
        content = content.push(parts::progress(here, Step::COUNTED));
    }
    let content = content.push(head).push(blocks);

    let main = column![
        centred(content.into(), Length::Fill, [space::PANE_Y, space::PANE_X]),
        hairline(),
        centred(footer(m), Length::Shrink, [0.0, space::PANE_X]),
    ];

    container(main.width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window(eclipse_ui::tokens::radius::WINDOW))
        .into()
}

/// A column no wider than the content measure, centred in what is left.
fn centred<'a>(inner: El<'a, Message>, height: Length, padding: [f32; 2]) -> El<'a, Message> {
    container(
        container(inner)
            .max_width(metrics::CONTENT_W)
            .width(Length::Fill)
            .height(height),
    )
    .center_x(Length::Fill)
    .height(height)
    .padding(padding)
    .into()
}

fn footer(m: &Model) -> El<'_, Message> {
    let back_label = if m.step == Step::Install {
        "Try again"
    } else {
        "Back"
    };
    let back: El<'_, Message> = if m.can_back() {
        parts::ghost(back_label, Some(Message::Back))
    } else {
        Space::new().width(Length::Shrink).into()
    };

    let forward: El<'_, Message> = match m.step {
        Step::Review => parts::commit("Erase and install", m.can_apply().then_some(Message::Apply)),
        Step::Install => match m.install.phase {
            Phase::Done => parts::commit(
                if m.install.restarting {
                    "Restarting"
                } else {
                    "Restart"
                },
                (!m.install.restarting).then_some(Message::Restart),
            ),
            Phase::Failed => Space::new().width(Length::Shrink).into(),
            _ => parts::primary("Installing", None),
        },
        Step::Network if !m.online() => parts::primary("Skip", m.can_next().then_some(Message::Next)),
        Step::Welcome => Space::new().width(Length::Shrink).into(),
        _ => parts::primary("Continue", m.can_next().then_some(Message::Next)),
    };

    container(
        row![back, Space::new().width(Length::Fill), forward]
            .spacing(space::CARD)
            .align_y(Alignment::Center),
    )
    .height(Length::Fixed(metrics::FOOTER_H))
    .align_y(Alignment::Center)
    .into()
}

// ------------------------------------------------------------ shared blocks

/// A list on the inset ground, filling the height it is given. `top` is an
/// optional block above the rows (a filter field).
pub(crate) fn list_box<'a>(top: Option<El<'a, Message>>, rows: Column<'a, Message>) -> El<'a, Message> {
    let mut col = Column::new().spacing(space::CHIP_GAP);
    if let Some(t) = top {
        col = col.push(t);
    }
    col = col.push(
        scrollable(container(rows).padding([0.0, space::CHIP_GAP]))
            .style(theme::eclipse_scrollable)
            .width(Length::Fill)
            .height(Length::Fill),
    );
    eclipse_ui::widget::inset(col)
        .padding(space::CARD)
        .height(Length::Fill)
        .into()
}

/// Ink for a reading that is fine.
pub(crate) fn ok_ink() -> iced::Color {
    color::OK
}
