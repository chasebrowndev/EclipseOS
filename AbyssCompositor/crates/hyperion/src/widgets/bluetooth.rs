// SPDX-License-Identifier: AGPL-3.0-only
//! Bluetooth: the rune as the core, the device drawer behind it.
//!
//! A status widget: it reads the app's status fields (shared with the
//! drawers), so it has no `State` or `update` of its own.

use iced::{Color, Element, Theme};

use eclipse_services::status::Bluetooth;
use eclipse_ui::tokens::{bar, color, drawer};
use eclipse_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::view::{elide, Sheet, DRAWER_CHARS};

pub fn spans(_app: &App) -> Spans {
    Spans {
        core: bar::MARK,
        revealed: 0.0,
        present: true,
    }
}

pub fn view(app: &App, _frame: ShellFrame) -> Parts<'_> {
    Parts {
        core: super::press(
            bluetooth_mark(app.bluetooth, bar::MARK),
            bar::MARK,
            Message::Open(Drawer::Bluetooth),
        ),
        revealed: None,
    }
}

pub(crate) fn glyph(bt: Bluetooth) -> &'static str {
    match (bt.powered, bt.connected) {
        (false, _) => "bluetooth-disabled",
        (true, 0) => "bluetooth-disconnected",
        (true, _) => "bluetooth-active",
    }
}

/// The bluetooth mark: the theme's own rune, in one of three hues — inert
/// grey when the radio is off, the accent when it is powered but idle, and
/// [`color::CONNECTED`] when a device is on the other end. The glyph changes
/// with it so the state survives for anyone who cannot tell the hues apart.
pub(crate) fn bluetooth_mark(bt: Bluetooth, side: f32) -> Element<'static, Message, Theme> {
    let tint = match (bt.powered, bt.connected) {
        (false, _) => color::NEUTRAL,
        (true, 0) => color::ACCENT,
        (true, _) => color::CONNECTED,
    };
    parts::mark(crate::icons::symbolic(glyph(bt)), side, tint)
}

/// A bluetooth device's mark, by what the device is.
fn device_mark(kind: crate::radio::BtKind, side: f32, tint: Color) -> Element<'static, Message, Theme> {
    use crate::radio::BtKind;
    let name = match kind {
        BtKind::Audio => "audio-headphones",
        BtKind::Input => "input-mouse",
        BtKind::Phone => "phone",
        BtKind::Other => "bluetooth-active",
    };
    parts::mark(crate::icons::symbolic(name), side, tint)
}

/// The bluetooth drawer. See `view::drawer_view` for its anatomy and ledger.
pub(crate) fn sheet(app: &App) -> Sheet {
    let on = app.bluetooth.powered;
    let mut s = Sheet::new(
        parts::drawer_switch_head("bluetooth", parts::Toggle::new(on, Message::BtPower)),
        drawer::HEAD_SWITCH_H,
    );
    if !on {
        s.row(parts::drawer_note("off"));
    } else {
        let devices = &app.radios.devices;
        let connected: Vec<_> = devices.iter().filter(|d| d.connected).collect();
        let paired: Vec<_> = devices
            .iter()
            .filter(|d| d.paired && !d.connected)
            .take(drawer::MAX_ROWS)
            .collect();
        for d in &connected {
            s.current(parts::drawer_current(
                device_mark(d.kind, drawer::CURRENT_MARK, color::CONNECTED),
                &elide(&d.name, DRAWER_CHARS),
                "connected",
                color::CONNECTED,
                Some(("Disconnect", Message::BtDisconnect(d.addr.clone()))),
            ));
        }
        if !connected.is_empty() && !paired.is_empty() {
            s.rule();
        }
        for d in &paired {
            s.row(parts::drawer_choice(
                device_mark(d.kind, drawer::MARK, color::TEXT_SECONDARY),
                &elide(&d.name, DRAWER_CHARS),
                None,
                Some(Message::BtConnect(d.addr.clone())),
            ));
        }
        if connected.is_empty() && paired.is_empty() {
            s.row(parts::drawer_note("no paired devices"));
        }
        s.rule();
        // Discovery is a verb on the list rather than a second switch: it
        // runs for as long as the human is looking, and a toggle would claim
        // it is a setting that stays on.
        let scanning = app.radios.scanning;
        s.row(parts::drawer_choice(
            parts::mark(
                crate::icons::symbolic("view-refresh"),
                drawer::MARK,
                color::TEXT_SECONDARY,
            ),
            if scanning {
                "Stop scanning"
            } else {
                "Scan for devices"
            },
            None,
            Some(Message::BtScan),
        ));
        if scanning {
            let nearby: Vec<_> = devices
                .iter()
                .filter(|d| !d.paired)
                .take(drawer::MAX_ROWS)
                .collect();
            for d in &nearby {
                s.row(parts::drawer_choice(
                    device_mark(d.kind, drawer::MARK, color::TEXT_TERTIARY),
                    &elide(&d.name, DRAWER_CHARS),
                    None,
                    Some(Message::BtConnect(d.addr.clone())),
                ));
            }
            if nearby.is_empty() {
                s.row(parts::drawer_note("searching\u{2026}"));
            }
        }
    }
    s.rule();
    s.row(parts::drawer_link(
        "Bluetooth settings",
        Message::OpenSettings("network"),
    ));
    s
}
