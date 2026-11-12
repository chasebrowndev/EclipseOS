// SPDX-License-Identifier: AGPL-3.0-only

//! The picker's bottom bar (FOG §File picker): the name field (save), the
//! choices, the filter dropdown, the selection's summary, Cancel and the
//! accept button; and the dropdown menu that opens over it.
//!
//! The bar is one flat band under the whole window, parted from the table by
//! a hairline, not a card: inset fields and glass lozenges on the window's own
//! tint. Focus and selection are a brighter fill and a crisp rim, never gold:
//! the gold stays with the cursor (or the name's caret while it is typed in).

use iced::widget::{button, column, container, row, text, Column, Space};
use iced::{alignment, Background, Border, Color, Element, Length, Padding};

use super::{caption, field_style, label, pill, rule_h, scrim};
use crate::app::{App, Message};
use crate::pick::{ChoiceValue, PickMode, PickRequest};
use crate::picker::{self, Menu, Of, Ui};
use crate::state::Browser;
use crate::theme::{color, font, look, size};
use ec_fog_widgets::elide;

/// The bar for the active listing, if this is a picker.
pub fn bar(app: &App) -> Option<Element<'_, Message>> {
    let (req, ui) = (app.pick.as_ref()?, app.pickui.as_ref()?);
    let b = app.tabs.active();
    let mut col = Column::new().spacing(size::INSET);
    if let Some(top) = top_row(req, ui, b) {
        col = col.push(top);
    }
    if let Some(r) = choices_row(req, ui) {
        col = col.push(r);
    }
    col = col.push(action_row(req, ui, b));
    Some(
        column![
            rule_h(1.0),
            container(col)
                .padding([size::INSET, size::PAD_X - size::PILL_X])
                .width(Length::Fill)
        ]
        .into(),
    )
}

fn chevron<'a>(fg: Color) -> text::Text<'a> {
    label("▾", size::TEXT, fg)
}

/// A control's wash: rest, hover and press, with the edge each takes. A part
/// of this bar because iced's `button` brings its own filled look.
fn ctl(
    rest: (Color, Color),
    hover: (Color, Color),
) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    let r = look().inset_radius;
    move |_, status| {
        let (fill, edge) = match status {
            button::Status::Hovered | button::Status::Pressed => hover,
            _ => rest,
        };
        button::Style {
            background: Some(Background::Color(fill)),
            border: Border {
                color: edge,
                width: if edge.a > 0.0 { size::HAIRLINE } else { 0.0 },
                radius: r.into(),
            },
            text_color: color::TEXT,
            ..Default::default()
        }
    }
}

/// An inset dropdown button: its current value, then the chevron.
fn dropdown<'a>(
    prefix: Option<&str>,
    value: String,
    open: bool,
    on: Message,
) -> Element<'a, Message> {
    let mut r = row![]
        .spacing(size::GLYPH_GAP)
        .align_y(alignment::Vertical::Center);
    if let Some(p) = prefix {
        r = r.push(caption(
            p.to_owned(),
            size::TEXT_SMALL,
            color::TEXT_TERTIARY,
        ));
    }
    r = r
        .push(
            elide(value)
                .size(size::TEXT)
                .font(font::UI)
                .color(color::TEXT),
        )
        .push(chevron(color::TEXT_SECONDARY));
    let rest = if open {
        (color::MARK_FILL, color::RIM)
    } else {
        (color::FIELD, color::BORDER)
    };
    button(container(r).width(Length::Fill).center_y(Length::Fill))
        .width(size::DROP_W)
        .height(size::INPUT_H)
        .padding([0.0, size::PAD_X - size::PILL_X])
        .on_press(on)
        .style(ctl(rest, (color::MARK_FILL, color::BORDER_STRONG)))
        .into()
}

/// Save: the name field. Save-files: the names, read-only.
fn top_row<'a>(req: &'a PickRequest, ui: &'a Ui, b: &'a Browser) -> Option<Element<'a, Message>> {
    let _ = b;
    let head = |s: &'static str| {
        container(caption(s, size::CAPTION, color::TEXT_TERTIARY))
            .width(size::BAR_LABEL_W)
            .align_y(alignment::Vertical::Center)
            .height(size::INPUT_H)
    };
    match req.mode {
        PickMode::Save => {
            let (before, after) = ui.name.halves();
            let caret = if ui.typing {
                color::ACCENT
            } else {
                color::TEXT_TERTIARY
            };
            let line = row![
                label(before.to_owned(), size::TEXT, color::TEXT),
                super::block(size::CARET_W, size::TEXT, Some(caret)),
                label(after.to_owned(), size::TEXT, color::TEXT),
            ]
            .align_y(alignment::Vertical::Center);
            let typing = ui.typing;
            let rest = if typing {
                (color::MARK_FILL, color::RIM)
            } else {
                (color::FIELD, color::BORDER)
            };
            let name = button(
                container(line)
                    .clip(true)
                    .width(Length::Fill)
                    .center_y(Length::Fill),
            )
            .width(Length::Fill)
            .height(size::INPUT_H)
            .padding([0.0, size::PAD_X - size::PILL_X])
            .on_press(Message::PickName)
            .style(ctl(rest, (color::MARK_FILL, color::BORDER_STRONG)));
            Some(
                row![head("name"), name]
                    .align_y(alignment::Vertical::Center)
                    .into(),
            )
        }
        PickMode::SaveFiles => {
            let names: Vec<String> = req
                .files
                .iter()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect();
            let listing = container(
                elide(names.join(", "))
                    .size(size::MONO)
                    .font(font::DATA)
                    .color(color::TEXT_SECONDARY),
            )
            .padding([0.0, size::PAD_X - size::PILL_X])
            .width(Length::Fill)
            .height(size::INPUT_H)
            .align_y(alignment::Vertical::Center)
            .clip(true)
            .style(field_style());
            Some(
                row![head("saving"), listing]
                    .align_y(alignment::Vertical::Center)
                    .into(),
            )
        }
        _ => None,
    }
}

/// The app's choices: a dropdown per combo, a box per checkbox.
fn choices_row<'a>(req: &'a PickRequest, ui: &'a Ui) -> Option<Element<'a, Message>> {
    if req.choices.is_empty() {
        return None;
    }
    let mut r = row![]
        .spacing(size::INSET)
        .align_y(alignment::Vertical::Center);
    for (i, c) in req.choices.iter().enumerate() {
        r = r.push(match ui.choices.get(i) {
            Some(ChoiceValue::Bool(on)) => checkbox(&c.label, *on, i),
            _ => {
                let of = Of::Choice(i);
                let value = ui
                    .options(req, of)
                    .get(ui.current(of))
                    .map_or_else(String::new, |s| (*s).to_owned());
                dropdown(
                    Some(&c.label),
                    value,
                    ui.menu.is_some_and(|m| m.of == of),
                    Message::PickMenu(of),
                )
            }
        });
    }
    Some(r.into())
}

/// A box and its label. On is a clearer fill with a crisp rim and an inner
/// block, all neutral; hover washes the whole control.
fn checkbox<'a>(name: &str, on: bool, i: usize) -> Element<'a, Message> {
    let r = look().chip_radius;
    let boxed = container(if on {
        container(Space::new())
            .width(size::CHECK - 6.0)
            .height(size::CHECK - 6.0)
            .style(move |_| container::Style {
                background: Some(Background::Color(color::TEXT)),
                border: Border {
                    radius: size::GLYPH_R.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
    } else {
        container(Space::new())
    })
    .center(size::CHECK)
    .style(move |_| {
        let (fill, edge) = if on {
            (color::LIFT_STRONG, color::RIM)
        } else {
            (color::FIELD, color::BORDER_STRONG)
        };
        pill(fill, edge, None, size::GLYPH_R)
    });
    button(
        container(
            row![
                boxed,
                caption(name.to_owned(), size::TEXT, color::TEXT_SECONDARY)
            ]
            .spacing(size::GLYPH_GAP)
            .align_y(alignment::Vertical::Center),
        )
        .center_y(Length::Fill),
    )
    .height(size::INPUT_H)
    .padding([0.0, size::PAD_X - size::PILL_X])
    .on_press(Message::PickToggle(i))
    .style(move |_, status| {
        let fill = match status {
            button::Status::Hovered => color::HOVER,
            button::Status::Pressed => color::MARK_FILL,
            _ => Color::TRANSPARENT,
        };
        button::Style {
            background: Some(Background::Color(fill)),
            border: Border {
                radius: r.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
    .into()
}

/// A text button: `primary` is the clear lozenge, `danger` the replace one.
fn action<'a>(
    word: String,
    on: Option<Message>,
    primary: bool,
    danger: bool,
) -> Element<'a, Message> {
    let enabled = on.is_some();
    let fg = match (enabled, primary) {
        (false, _) => color::TEXT_TERTIARY,
        (true, true) => color::TEXT,
        (true, false) => color::TEXT_SECONDARY,
    };
    let (rest, hover) = match (enabled, primary, danger) {
        (_, _, true) => (
            (color::DANGER_FILL, color::DANGER_BORDER),
            (color::DANGER_FILL, color::DANGER),
        ),
        (false, _, _) => ((color::FIELD, color::BORDER), (color::FIELD, color::BORDER)),
        (true, true, _) => (
            (color::LIFT_STRONG, color::RIM),
            (color::LIFT_HOVER, color::RIM),
        ),
        (true, false, _) => (
            (Color::TRANSPARENT, color::BORDER),
            (color::MARK_FILL, color::BORDER_STRONG),
        ),
    };
    let fg = if danger { color::DANGER } else { fg };
    let b = button(
        container(caption(word, size::TEXT, fg).font(if primary {
            font::UI_MEDIUM
        } else {
            font::UI
        }))
        .center_y(Length::Fill),
    )
    .height(size::INPUT_H)
    .padding([0.0, size::PAD_X + size::PILL_X])
    .style(ctl(rest, hover));
    match on {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

/// The filter, what will be answered, Cancel and Accept: or, while save
/// asks about a name that exists, that question.
fn action_row<'a>(req: &'a PickRequest, ui: &'a Ui, b: &'a Browser) -> Element<'a, Message> {
    if let Some(target) = &ui.replace {
        let name = crate::view::basename(target);
        return row![
            caption(
                format!("“{name}” already exists. Replace it?"),
                size::TEXT,
                color::TEXT,
            ),
            caption(
                "y replace · esc keep",
                size::TEXT_SMALL,
                color::TEXT_TERTIARY
            ),
            Space::new().width(Length::Fill),
            action(
                "Keep looking".into(),
                Some(Message::PickReplace(false)),
                false,
                false
            ),
            action(
                "Replace".into(),
                Some(Message::PickReplace(true)),
                false,
                true
            ),
        ]
        .spacing(size::INSET)
        .align_y(alignment::Vertical::Center)
        .height(size::INPUT_H)
        .into();
    }
    let mut r = row![]
        .spacing(size::INSET)
        .align_y(alignment::Vertical::Center);
    if !req.filters.is_empty() {
        let name = ui
            .filter
            .and_then(|i| req.filters.get(i))
            .map_or_else(String::new, |f| f.name.clone());
        r = r.push(dropdown(
            None,
            name,
            ui.menu.is_some_and(|m| m.of == Of::Filter),
            Message::PickMenu(Of::Filter),
        ));
    }
    let (said, fg) = match ui.hint {
        Some(h) => (h.to_owned(), color::DANGER),
        None => (picker::summary(req, ui, b), color::TEXT_SECONDARY),
    };
    r = r.push(
        container(elide(said).size(size::TEXT_SMALL).font(font::UI).color(fg))
            .padding([0.0, size::PILL_X])
            .width(Length::Fill)
            .clip(true),
    );
    let ready = ui.paths(req, b).is_ok();
    r.push(action(
        "Cancel".into(),
        Some(Message::PickCancel),
        false,
        false,
    ))
    .push(action(
        picker::accept_label(req),
        ready.then_some(Message::PickAccept),
        true,
        false,
    ))
    .height(size::INPUT_H)
    .into()
}

/// The open dropdown over the bar: a click-away layer, then the menu on its
/// solid ground, anchored above its button. The keyboard's row is the one
/// gold wash; the row in use is a neutral one.
pub fn menu<'a>(app: &'a App, m: Menu) -> Option<Element<'a, Message>> {
    let (req, ui) = (app.pick.as_ref()?, app.pickui.as_ref()?);
    let r = look().chip_radius;
    let current = ui.current(m.of);
    let mut col = Column::new();
    for (i, name) in ui.options(req, m.of).into_iter().enumerate() {
        let cursor = i == m.at;
        let used = i == current;
        let fg = if cursor {
            color::ACCENT_TEXT
        } else {
            color::TEXT
        };
        let cell = container(
            elide(name.to_owned())
                .size(size::TEXT)
                .font(if used { font::UI_MEDIUM } else { font::UI })
                .color(fg),
        )
        .padding([0.0, size::PAD_X - size::PILL_X])
        .width(Length::Fill)
        .height(size::ROW_H)
        .align_y(alignment::Vertical::Center);
        col = col.push(
            button(cell)
                .padding(Padding::ZERO)
                .width(Length::Fill)
                .on_press(Message::PickSet(m.of, i))
                .style(move |_, status| {
                    let fill = match (cursor, used, status) {
                        (_, _, button::Status::Hovered | button::Status::Pressed) => {
                            color::MARK_FILL
                        }
                        (true, _, _) => color::ACCENT_FILL,
                        (_, true, _) => color::HOVER,
                        _ => Color::TRANSPARENT,
                    };
                    button::Style {
                        background: Some(Background::Color(fill)),
                        border: Border {
                            radius: r.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                }),
        );
    }
    let l = look();
    let panel = container(col)
        .padding(size::MENU_PAD)
        .width(size::DROP_W)
        .style(move |_| container::Style {
            background: Some(Background::Color(color::MENU_GROUND)),
            border: Border {
                color: color::BORDER_STRONG,
                width: size::HAIRLINE,
                radius: l.inset_radius.into(),
            },
            shadow: l.menu_shadow(),
            ..Default::default()
        });
    // The filter sits first in the action row, choices in the row above it.
    let slot = match m.of {
        Of::Filter => 0,
        Of::Choice(i) => i,
    };
    let mut above = size::INSET + size::INPUT_H + size::MENU_PAD;
    if matches!(m.of, Of::Choice(_)) {
        above += size::INSET + size::INPUT_H;
    }
    let at = Padding {
        top: 0.0,
        right: 0.0,
        bottom: above,
        left: size::PAD_X - size::PILL_X + slot as f32 * (size::DROP_W + size::INSET),
    };
    let placed = container(panel)
        .padding(at)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(alignment::Vertical::Bottom);
    Some(iced::widget::stack![scrim(Some(Message::PickMenu(m.of)), 0.0), placed].into())
}
