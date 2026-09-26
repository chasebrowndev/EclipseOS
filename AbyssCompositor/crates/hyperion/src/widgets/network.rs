// SPDX-License-Identifier: AGPL-3.0-only
//! Network: the signal cone as the core, the wi-fi drawer behind it.
//!
//! A status widget: it reads the app's status fields (shared with the
//! drawers), so it has no `State` or `update` of its own.

use iced::{Element, Theme};

use eclipse_services::status::Network;
use eclipse_ui::tokens::{bar, color, drawer};
use eclipse_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::view::{blank_mark, elide, Sheet, DRAWER_CHARS};

pub fn spans(_app: &App) -> Spans {
    Spans {
        core: bar::MARK,
        revealed: 0.0,
        present: true,
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    Parts {
        core: super::press(
            network_mark(&app.network, bar::MARK, frame.core_alpha()),
            bar::MARK,
            Message::Open(Drawer::Network),
        ),
        revealed: None,
    }
}

/// The icon-theme name for a link's state: the signal cone at its rung, or
/// the plain *connected* glyph where there is no magnitude.
///
/// Name only, no tint — the bar and the drawers colour the same glyph by
/// different ledgers.
pub(crate) fn glyph(network: &Network) -> &'static str {
    match network {
        Network::Offline => "network-wireless-offline",
        Network::Wired { .. } | Network::Other { .. } => "network-wireless-connected",
        Network::Wifi { strength, .. } => match strength {
            0..=10 => "network-wireless-signal-none",
            11..=35 => "network-wireless-signal-weak",
            36..=60 => "network-wireless-signal-ok",
            61..=80 => "network-wireless-signal-good",
            _ => "network-wireless-signal-excellent",
        },
    }
}

/// The network mark: the platform's own signal cone, tinted.
///
/// Real theme art rather than drawn bars — a stack of rising rectangles reads
/// as a cellular meter, and this is the arc every desktop uses for wifi. The
/// rungs are the icon theme's five `network-wireless-signal-*` levels; a
/// wired or unknown link gets the plain *connected* glyph because there is no
/// magnitude to report for it.
///
/// White ink only (the accent ledger in `view.rs`): a link is up or down, and
/// the rung of the cone already says how strong it is. `alpha` fades it with
/// its cell.
pub(crate) fn network_mark(network: &Network, side: f32, alpha: f32) -> Element<'static, Message, Theme> {
    let tint = match network {
        Network::Offline => color::TEXT_TERTIARY,
        _ => color::TEXT_SECONDARY,
    };
    parts::mark(
        crate::icons::symbolic(glyph(network)),
        side,
        tint.scale_alpha(alpha),
    )
}

/// The wi-fi drawer. See `view::drawer_view` for its anatomy and ledger.
pub(crate) fn sheet(app: &App) -> Sheet {
    let on = app.radios.wifi_enabled;
    let mut s = Sheet::new(
        parts::drawer_switch_head("wi-fi", parts::Toggle::new(on, Message::WifiEnable)),
        drawer::HEAD_SWITCH_H,
    );
    if !on {
        s.row(parts::drawer_note("off"));
    } else {
        // The service's list says which network is active; before it has
        // answered, the status feed's link id is the same fact.
        let current = app
            .radios
            .active_network()
            .map(|n| n.ssid.clone())
            .or(match &app.network {
                Network::Wifi { id, .. } => Some(id.clone()),
                _ => None,
            });
        if let Some(ssid) = &current {
            s.current(parts::drawer_current(
                parts::mark(
                    crate::icons::symbolic(glyph(&app.network)),
                    drawer::CURRENT_MARK,
                    color::CONNECTED,
                ),
                &elide(ssid, DRAWER_CHARS),
                "connected",
                color::CONNECTED,
                Some(("Disconnect", Message::WifiDisconnect)),
            ));
        }
        let others: Vec<_> = app
            .radios
            .networks
            .iter()
            .filter(|n| Some(&n.ssid) != current.as_ref())
            .take(drawer::MAX_ROWS)
            .collect();
        if current.is_some() && !others.is_empty() {
            s.rule();
        }
        for n in others {
            // A lock and nothing else: whether joining will ask for a secret
            // is the only thing about a stranger's network the drawer owes
            // the human before they click it.
            let lock = n.secured.then(|| {
                parts::mark(
                    crate::icons::symbolic("network-wireless-encrypted"),
                    drawer::MARK,
                    color::TEXT_TERTIARY,
                )
            });
            s.row(parts::drawer_choice(
                blank_mark(),
                &elide(&n.ssid, DRAWER_CHARS),
                lock,
                Some(Message::WifiConnect(n.ssid.clone())),
            ));
        }
        if current.is_none() && app.radios.networks.is_empty() {
            s.row(parts::drawer_note("searching\u{2026}"));
        }
    }
    s.rule();
    s.row(parts::drawer_link(
        "Network settings",
        Message::OpenSettings("network"),
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cell is a reading, not a guess: offline has its own glyph rather
    /// than the weakest rung, because a dead link and a faint one look the
    /// same on a bar and only one of them is worth telling the human about.
    #[test]
    fn an_offline_link_still_says_something() {
        assert_eq!(glyph(&Network::Offline), "network-wireless-offline");
    }

    #[test]
    fn wifi_reports_its_rung() {
        let wifi = Network::Wifi {
            id: "House".into(),
            strength: 49,
        };
        assert_eq!(glyph(&wifi), "network-wireless-signal-ok");
    }
}
