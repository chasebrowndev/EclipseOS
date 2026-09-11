// SPDX-License-Identifier: AGPL-3.0-only
//! The bar's one row.
//!
//! Left to right: workspace pips, the windows on the focused workspace, the
//! clock. Every colour and size comes from `eclipse_ui::tokens` — a literal
//! anywhere in this file is a bug, because the tokens are the only
//! transcription of `docs/STYLE.md`.

use iced::widget::{container, mouse_area, row, text, Row, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use eclipse_services::status::{Battery, Bluetooth, Charge, Network};
use eclipse_ui::tokens::{color, font, size};

use crate::app::Message;
use crate::model::{Snapshot, Window, Workspace};
use crate::HEIGHT;

/// Width of one workspace pip, matching the reference shell's cell metrics.
const PIP_W: f32 = 22.0;
/// Gap between the pieces inside a cell.
const CELL_GAP: f32 = 7.0;
/// Padding either side of a group of cells.
const EDGE: f32 = 11.0;
/// The bar's bottom rule. One pixel, and the only edge the bar draws.
const HAIRLINE: f32 = 1.0;

pub fn view(app: &crate::app::App) -> Element<'_, Message, Theme> {
    let snapshot = &app.snapshot;

    let bar = row![
        workspaces(snapshot),
        windows(snapshot),
        Space::new().width(Length::Fill),
        status(app),
        clock(snapshot),
    ]
    .spacing(CELL_GAP * 2.0)
    .padding([0.0, EDGE])
    .align_y(Alignment::Center)
    .height(Length::Fixed(HEIGHT as f32 - HAIRLINE));

    let body = container(bar)
        .width(Length::Fill)
        .style(|_t: &Theme| container::Style {
            background: Some(iced::Background::Color(color::BASE)),
            ..container::Style::default()
        });

    // A hairline at the bottom and nothing else: no rounding, no per-cell
    // borders. The bar is an edge of the screen, not a floating card.
    let rule = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(HAIRLINE))
        .style(|_t: &Theme| container::Style {
            background: Some(iced::Background::Color(color::HAIRLINE)),
            ..container::Style::default()
        });

    iced::widget::column![body, rule].into()
}

/// One pip per workspace. The active pip is the bar's single accented
/// element — everything else is text weight.
fn workspaces(snapshot: &Snapshot) -> Element<'_, Message, Theme> {
    let mut r = Row::new().align_y(Alignment::Center);
    for ws in &snapshot.workspaces {
        r = r.push(pip(ws));
    }
    r.into()
}

fn pip(ws: &Workspace) -> Element<'_, Message, Theme> {
    let (glyph, tint) = if ws.active {
        ("◉", color::ACCENT)
    } else if ws.windows > 0 {
        ("○", color::TEXT)
    } else {
        ("○", color::TEXT_TERTIARY)
    };
    let cell = container(text(glyph).size(size::BODY).color(tint))
        .width(Length::Fixed(PIP_W))
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
    mouse_area(cell).on_press(Message::Switch(ws.index)).into()
}

/// The windows on the focused workspace. Click focuses, middle-click closes.
fn windows(snapshot: &Snapshot) -> Element<'_, Message, Theme> {
    let active = snapshot.workspaces.iter().find(|w| w.active).map(|w| w.index);
    let mut r = Row::new().spacing(CELL_GAP).align_y(Alignment::Center);
    for w in &snapshot.windows {
        if active.is_some() && w.workspace != active {
            continue;
        }
        r = r.push(entry(w, snapshot.focused == Some(w.handle)));
    }
    r.into()
}

fn entry(w: &Window, focused: bool) -> Element<'_, Message, Theme> {
    // The focused entry reads as primary text, not as a second yellow: the
    // accent belongs to the active workspace alone.
    let tint = if focused {
        color::TEXT
    } else {
        color::TEXT_SECONDARY
    };
    let face = if focused { font::UI_MEDIUM } else { font::UI };
    let cell = container(
        // `label()` and nothing else: it is what hides a `secret` window's
        // client-controlled title.
        text(w.label().to_owned())
            .size(size::BODY_SMALL)
            .color(tint)
            .font(face),
    )
    .padding([0, CELL_GAP as u16])
    .height(Length::Fill)
    .align_y(Alignment::Center);
    mouse_area(cell)
        .on_press(Message::Focus(w.handle))
        .on_middle_press(Message::Close(w.handle))
        .into()
}

/// Network, bluetooth and battery, in that order, to the left of the clock.
///
/// Every cell is words in the mono face rather than an icon: the bar ships its
/// own font faces (`eclipse_ui::FONTS`) and none of them is a Nerd Font, so a
/// glyph here would render as a box on the machine it is supposed to inform.
/// A cell that has nothing to say draws nothing at all — an empty gap is
/// honest, a zero is not.
fn status(app: &crate::app::App) -> Element<'_, Message, Theme> {
    let mut r = Row::new().spacing(CELL_GAP * 2.0).align_y(Alignment::Center);
    r = r.push(reading(
        network_text(&app.network),
        app.network != Network::Offline,
    ));
    if let Some(text) = bluetooth_text(app.bluetooth) {
        r = r.push(reading(text, app.bluetooth.connected > 0));
    }
    if let Some(battery) = app.battery {
        // Low and not charging is the one status the human has to act on, so
        // it is the one status allowed to leave the neutral palette.
        let cell = if battery.percent <= LOW && battery.state == Charge::Discharging {
            container(
                text(battery_text(battery))
                    .size(size::MONO)
                    .font(font::DATA)
                    .color(color::DANGER),
            )
        } else {
            container(
                text(battery_text(battery))
                    .size(size::MONO)
                    .font(font::DATA)
                    .color(color::TEXT_SECONDARY),
            )
        };
        r = r.push(cell.height(Length::Fill).align_y(Alignment::Center));
    }
    r.into()
}

/// Percentage at or below which a discharging battery is drawn as a warning.
const LOW: u8 = 15;

fn reading(body: String, live: bool) -> Element<'static, Message, Theme> {
    // Present but idle reads as secondary; absent or off reads as tertiary.
    // Neither is the accent: that belongs to the active workspace alone.
    let tint = if live {
        color::TEXT_SECONDARY
    } else {
        color::TEXT_TERTIARY
    };
    container(text(body).size(size::MONO).font(font::DATA).color(tint))
        .height(Length::Fill)
        .align_y(Alignment::Center)
        .into()
}

/// Signal strength is shown as a number, not as bars: the bar has no icon set,
/// and "49" is more use than four boxes anyway.
fn network_text(network: &Network) -> String {
    match network {
        Network::Offline => "offline".to_owned(),
        Network::Wired { .. } => "wired".to_owned(),
        Network::Wifi { strength, .. } => format!("wifi {strength}"),
        Network::Other { .. } => "net".to_owned(),
    }
}

/// A powered-down adapter draws nothing. Bluetooth being off is the ordinary
/// state on most machines and is not news.
fn bluetooth_text(bluetooth: Bluetooth) -> Option<String> {
    if !bluetooth.powered {
        return None;
    }
    Some(match bluetooth.connected {
        0 => "bt".to_owned(),
        n => format!("bt {n}"),
    })
}

/// Charging is a leading `+`, discharging bare, full the word. Time remaining
/// is deliberately absent: it is the least trustworthy number UPower reports.
fn battery_text(battery: Battery) -> String {
    match battery.state {
        Charge::Charging => format!("+{}%", battery.percent),
        Charge::Full => "full".to_owned(),
        Charge::Discharging | Charge::Unknown => format!("{}%", battery.percent),
    }
}

/// The clock, and the one place the bar admits the compositor is gone.
fn clock(snapshot: &Snapshot) -> Element<'_, Message, Theme> {
    let tint = if snapshot.connected {
        color::TEXT
    } else {
        color::TEXT_TERTIARY
    };
    let time = if snapshot.clock.is_empty() {
        crate::clock::time()
    } else {
        snapshot.clock.clone()
    };
    container(text(time).size(size::BODY_SMALL).font(font::DATA).color(tint))
        .height(Length::Fill)
        .align_y(Alignment::Center)
        .into()
}

/// The bar draws on nothing: the layer surface itself is transparent, and the
/// opaque ground is the container in [`view`].
pub fn style(_app: &crate::app::App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cell is a reading, not a guess: offline says so in a word rather
    /// than disappearing, because a missing cell and a dead link look the same
    /// on a bar and only one of them is worth telling the human about.
    #[test]
    fn an_offline_link_still_says_something() {
        assert_eq!(network_text(&Network::Offline), "offline");
    }

    #[test]
    fn wifi_reports_its_strength() {
        assert_eq!(
            network_text(&Network::Wifi {
                id: "House".into(),
                strength: 49,
            }),
            "wifi 49"
        );
    }

    /// An adapter that is off is the normal case and must not take up a cell.
    #[test]
    fn a_powered_down_adapter_draws_nothing() {
        assert_eq!(
            bluetooth_text(Bluetooth {
                powered: false,
                connected: 0,
            }),
            None
        );
        assert_eq!(
            bluetooth_text(Bluetooth {
                powered: true,
                connected: 2,
            })
            .as_deref(),
            Some("bt 2")
        );
    }

    #[test]
    fn charging_is_distinguishable_from_draining() {
        let at = |state| Battery {
            percent: 80,
            state,
            remaining: None,
        };
        assert_eq!(battery_text(at(Charge::Charging)), "+80%");
        assert_eq!(battery_text(at(Charge::Discharging)), "80%");
        assert_eq!(battery_text(at(Charge::Full)), "full");
    }
}
