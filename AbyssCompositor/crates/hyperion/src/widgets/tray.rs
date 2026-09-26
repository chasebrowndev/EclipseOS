// SPDX-License-Identifier: AGPL-3.0-only
//! The tray: StatusNotifierItems and nothing else (ADR 0065). Network,
//! bluetooth, battery and volume are widgets of their own now, placed by
//! `bar.widgets.order`; a built-in id left in `bar.tray.*` still loads, and
//! is ignored here.
//!
//! The core is the pinned items (`bar.tray.pinned`, in its order; unset pins
//! none) followed by the disclosure arrow, which opens the overflow drawer
//! of every item that is neither pinned nor hidden. With no visible items the
//! widget has nothing to show and takes no room.
//!
//! A status widget: it reads `app.radios.tray` and `app.tray`, so it has no
//! `State` of its own.

use iced::widget::{button, container, image, mouse_area, Row};
use iced::{Alignment, Color, Element, Length, Theme};

use eclipse_ui::tokens::{bar, color, drawer};
use eclipse_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::conn::TrayConfig;
use crate::radio::{TrayIcon, TrayItem};
use crate::view::{elide, Sheet, DRAWER_CHARS};

/// Split the items into `(pinned, overflow)`, by index into `items`.
///
/// Pinned follows `bar.tray.pinned` order; the overflow keeps arrival order.
/// `bar.tray.hidden` wins over both. Ids are app names and need not be
/// unique, so an id pins every item that carries it.
pub(crate) fn split(items: &[TrayItem], cfg: &TrayConfig) -> (Vec<usize>, Vec<usize>) {
    let visible: Vec<usize> = (0..items.len())
        .filter(|&i| !cfg.hidden.iter().any(|h| *h == items[i].id))
        .collect();
    let mut pinned = Vec::new();
    for id in cfg.pinned.iter().flatten() {
        for &i in &visible {
            if items[i].id == *id && !pinned.contains(&i) {
                pinned.push(i);
            }
        }
    }
    let overflow = visible.into_iter().filter(|i| !pinned.contains(i)).collect();
    (pinned, overflow)
}

fn marks_w(n: usize) -> f32 {
    n as f32 * (bar::TRAY_MARK_W + bar::TRAY_GAP)
}

pub fn spans(app: &App) -> Spans {
    let (pinned, overflow) = split(&app.radios.tray, &app.tray);
    Spans {
        core: marks_w(pinned.len()) + bar::ARROW_W,
        revealed: 0.0,
        present: !pinned.is_empty() || !overflow.is_empty(),
    }
}

/// The arrow's span inside the widget's core, from the core's right end:
/// the overflow drawer hangs off it.
pub(crate) const ARROW_FROM_RIGHT: f32 = bar::ARROW_W;

pub fn view(app: &App, _frame: ShellFrame) -> Parts<'_> {
    let (pinned, _) = split(&app.radios.tray, &app.tray);
    let mut r = Row::new().spacing(bar::TRAY_GAP).align_y(Alignment::Center);
    for i in pinned {
        let Some(item) = app.radios.tray.get(i) else {
            continue;
        };
        // An application's tray art is recoloured to the tray's ink like
        // every other mark: a row of vendor-coloured logos is the one place
        // a desktop loses its own palette to its guests.
        let press = button(super::fixed(
            tray_mark(&item.icon, bar::MARK, color::TEXT_SECONDARY),
            bar::TRAY_MARK_W,
        ))
        .width(Length::Fixed(bar::TRAY_MARK_W))
        .height(Length::Fixed(bar::WIDGET_H))
        .padding(0)
        .style(super::inner)
        .on_press(Message::TrayActivate(item.address.clone()));
        r = r.push(mouse_area(press).on_right_press(Message::TrayMenu(item.address.clone())));
    }
    r = r.push(disclosure());
    Parts {
        core: r.into(),
        revealed: None,
    }
}

/// The arrow that opens the overflow drawer.
fn disclosure() -> Element<'static, Message, Theme> {
    button(
        container(parts::chevron(
            bar::ARROW,
            bar::ARROW_STROKE,
            color::TEXT_SECONDARY,
        ))
        .center(Length::Fill),
    )
    .width(Length::Fixed(bar::ARROW_W))
    .height(Length::Fixed(bar::WIDGET_H))
    .padding(0)
    .style(super::inner)
    .on_press(Message::Open(Drawer::Overflow))
    .into()
}

/// A tray item's art at `side`, in `tint`'s ink. An SVG is recoloured by
/// `parts::mark`; pixels were silhouetted when the item arrived, so here they
/// only take the ink's alpha. The handle is cloned, not built: its id is what
/// keeps the texture cached.
pub(crate) fn tray_mark(icon: &TrayIcon, side: f32, tint: Color) -> Element<'static, Message, Theme> {
    match icon {
        TrayIcon::Svg(path) => parts::mark(Some(path.clone()), side, tint),
        TrayIcon::Image(handle) => image(handle.clone())
            .width(Length::Fixed(side))
            .height(Length::Fixed(side))
            .opacity(tint.a)
            .into(),
        TrayIcon::None => parts::mark(None, side, tint),
    }
}

/// The overflow drawer: every item that is neither pinned nor hidden, on a
/// mark rail. No yellow — see `view::drawer_view`'s ledger.
pub(crate) fn sheet(app: &App) -> Sheet {
    let mut s = Sheet::new(parts::drawer_sheet_head("tray"), drawer::HEAD_H);
    let (_, overflow) = split(&app.radios.tray, &app.tray);
    if overflow.is_empty() {
        s.row(parts::drawer_note("nothing here"));
    }
    for i in overflow {
        let Some(item) = app.radios.tray.get(i) else {
            continue;
        };
        s.row(
            mouse_area(parts::drawer_choice(
                tray_mark(&item.icon, drawer::MARK, color::TEXT_SECONDARY),
                &elide(&item.title, DRAWER_CHARS),
                None,
                Some(Message::TrayActivate(item.address.clone())),
            ))
            .on_right_press(Message::TrayMenu(item.address.clone()))
            .into(),
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> TrayItem {
        TrayItem {
            id: id.into(),
            address: format!(":1.{id}"),
            title: id.into(),
            icon: TrayIcon::None,
        }
    }

    #[test]
    fn the_tray_follows_its_config_and_ignores_builtins() {
        let items = [item("steam"), item("discord"), item("nm-applet")];
        let cfg = TrayConfig {
            // `network` is a built-in id: a legacy config's, and ignored.
            pinned: Some(vec!["network".into(), "discord".into(), "steam".into()]),
            hidden: vec!["nm-applet".into(), "battery".into()],
        };
        assert_eq!(split(&items, &cfg), (vec![1, 0], vec![]));
        // Unset pins nothing: everything visible waits behind the arrow.
        let (pinned, overflow) = split(&items, &TrayConfig::default());
        assert!(pinned.is_empty());
        assert_eq!(overflow, vec![0, 1, 2]);
    }
}
