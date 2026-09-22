// SPDX-License-Identifier: AGPL-3.0-only
//! The Network pane: the current wifi link in full, and the two things the
//! taskbar's drawers cannot do — forget a saved network, forget a device.
//!
//! Not schema keys. The readings come from the status service's action handle
//! (`eclipse_services::status`), on a thread that exists only while this pane
//! is showing; nothing here touches `abyss.kdl`.
//!
//! Shape: header (status chip) → hero (magnitude: the signal, then the link's
//! readings as a grid) → one row of two inset lists. The hero is a number and
//! a meter over a grid; the lists are hairlined rows side by side — no two
//! adjacent blocks share a silhouette.
//!
//! Accent ledger: the one yellow is the signal percentage. The meter is the
//! same reading drawn, so it shares the accent rather than spending a second
//! one; every pill here is unaccented.

use std::fmt;
use std::time::{Duration, Instant};

use iced::widget::{column, container, row, text, Column, Row, Space};
use iced::{Alignment, Element, Length, Subscription, Theme};

use eclipse_services::status::{self, Actions, BtDevice, Event, PairingAgent, Security, WifiDetail};
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget::{
    big_value, hairline, inset_list, list_row, meter_bar, micro_label, panel, pill, value as mono,
};

use crate::app::Message;

/// How long the feed thread waits for an event before checking whether it is
/// due to re-read. Also the longest it outlives the pane after a switch away.
const TICK: Duration = Duration::from_millis(250);
/// The link detail is re-read this often. The service samples rates over one
/// second per read, so faster than this would only queue reads.
const DETAIL_EVERY: Duration = Duration::from_secs(2);
/// Saved profiles and devices change on human timescales.
const LISTS_EVERY: Duration = Duration::from_secs(8);

/// The action handle, carried through a `Message`. A newtype because
/// `Actions` is not `Debug` and `Message` is.
#[derive(Clone)]
pub struct Handle(pub Actions);

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Handle")
    }
}

/// What the pane knows. Empty until the feed's first answers arrive.
#[derive(Default)]
pub struct Net {
    actions: Option<Actions>,
    /// `None` inside: asked, and no wifi link is up. Outer `None`: not yet
    /// answered.
    detail: Option<Option<WifiDetail>>,
    saved: Option<Vec<String>>,
    devices: Option<Vec<BtDevice>>,
    /// Why there is no service at all (no system bus).
    down: Option<String>,
    /// The last action that failed, in words.
    failure: Option<String>,
}

impl Net {
    pub fn ready(&mut self, handle: Handle) {
        self.actions = Some(handle.0);
        self.down = None;
    }

    pub fn down(&mut self, why: String) {
        self.actions = None;
        self.down = Some(why);
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::WifiDetail(d) => self.detail = Some(d),
            Event::WifiSaved(s) => self.saved = Some(s),
            Event::BtDevices(d) => self.devices = Some(d),
            Event::Failed { action, reason } => self.failure = Some(format!("{action}: {reason}")),
            _ => {}
        }
    }

    /// Forget a saved network. The row goes at once; the next list read is
    /// the truth and restores it if the service refused.
    pub fn forget_wifi(&mut self, ssid: String) {
        let Some(a) = &self.actions else { return };
        if let Some(s) = self.saved.as_mut() {
            s.retain(|x| *x != ssid);
        }
        self.failure = None;
        a.wifi_forget(ssid);
    }

    pub fn forget_device(&mut self, addr: String) {
        let Some(a) = &self.actions else { return };
        if let Some(d) = self.devices.as_mut() {
            d.retain(|x| x.addr != addr);
        }
        self.failure = None;
        a.bt_forget(addr);
    }

    /// The header chip: link state over interface and speed.
    pub fn chip(&self) -> (String, String) {
        if let Some(why) = &self.down {
            return ("no service".into(), why.clone());
        }
        match &self.detail {
            None => ("reading".into(), "—".into()),
            Some(None) => ("no wifi link".into(), "—".into()),
            Some(Some(d)) => (
                "wifi linked".into(),
                match d.link_speed_mbps {
                    Some(s) => format!("{} · {s} Mbps", d.interface),
                    None => d.interface.clone(),
                },
            ),
        }
    }
}

/// The feed: connect the action path, ask, forward every answer, re-ask on a
/// timer. Lives on its own thread because `actions()` blocks on the system
/// bus; it returns when the pane stops subscribing.
pub fn feed() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(32, async move |mut sender| {
            std::thread::spawn(move || {
                let (actions, events) = match status::actions(PairingAgent::None) {
                    Ok(pair) => pair,
                    Err(e) => {
                        send(&mut sender, Message::NetDown(e.to_string()));
                        return;
                    }
                };
                if !send(&mut sender, Message::NetReady(Handle(actions.clone()))) {
                    return;
                }
                let mut detail_at = Instant::now();
                let mut lists_at = Instant::now();
                actions.wifi_detail();
                actions.wifi_saved();
                actions.bt_devices();
                loop {
                    let alive = match events.recv_timeout(TICK) {
                        Some(ev) => send(&mut sender, Message::Net(ev)),
                        None => !sender.is_closed(),
                    };
                    if !alive {
                        return;
                    }
                    if detail_at.elapsed() >= DETAIL_EVERY {
                        detail_at = Instant::now();
                        actions.wifi_detail();
                    }
                    if lists_at.elapsed() >= LISTS_EVERY {
                        lists_at = Instant::now();
                        actions.wifi_saved();
                        actions.bt_devices();
                    }
                }
            });
        })
    })
}

/// Hand one message to the runtime. A full buffer drops it — the next read
/// replaces it — and only a closed one, the pane gone, reports `false`.
fn send(sender: &mut iced::futures::channel::mpsc::Sender<Message>, m: Message) -> bool {
    match sender.try_send(m) {
        Ok(()) => true,
        Err(e) => !e.is_disconnected(),
    }
}

/// The short name for what the link asks for.
fn security(s: Security) -> &'static str {
    match s {
        Security::Open => "open",
        Security::EnhancedOpen => "OWE",
        Security::Wep => "WEP",
        Security::WpaPersonal => "WPA2",
        Security::Wpa3Personal => "WPA3",
        Security::Enterprise => "802.1X",
    }
}

/// Bytes per second, in the largest unit that keeps a digit before the point.
fn rate(bps: u64) -> String {
    const UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
    let mut v = bps as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bps} {}", UNITS[0])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

fn joined(v: &[String]) -> String {
    if v.is_empty() {
        "—".into()
    } else {
        v.join(", ")
    }
}

fn tertiary<'a>(s: &str) -> Element<'a, Message, Theme> {
    text(s.to_string())
        .font(font::DATA)
        .size(size::MONO)
        .style(theme::text_tertiary)
        .into()
}

/// One reading: a caption over a mono value.
fn cell<'a>(label: &str, v: &str) -> Element<'a, Message, Theme> {
    container(column![micro_label(label), mono(v)].spacing(space::CHIP_Y))
        .width(Length::Fill)
        .into()
}

/// Cells in rows of `per`, each row the full width.
fn grid<'a>(cells: Vec<(&str, String)>, per: usize) -> Element<'a, Message, Theme> {
    let mut col = Column::new().spacing(space::ROW_Y);
    for chunk in cells.chunks(per) {
        let mut r = Row::new().spacing(space::CARD);
        for (label, v) in chunk {
            r = r.push(cell(label, v));
        }
        for _ in chunk.len()..per {
            r = r.push(Space::new().width(Length::Fill));
        }
        col = col.push(r);
    }
    col.into()
}

fn hero(net: &Net) -> Element<'_, Message, Theme> {
    let d = match &net.detail {
        Some(Some(d)) => d,
        other => {
            let (number, unit, why) = match (&net.down, other) {
                (Some(why), _) => ("—", "no service", why.as_str()),
                (None, None) => ("—", "reading", "asking the network service"),
                _ => ("—", "no link", "wifi is not connected"),
            };
            return panel(
                column![
                    micro_label("signal"),
                    big_value(number, unit, false),
                    meter_bar(space::HERO_METER_W, 0.0, color::TRACK),
                    tertiary(why),
                ]
                .spacing(space::ROW_Y),
            )
            .into();
        }
    };

    let band = d.band();
    let facts = if band.is_empty() {
        security(d.security).to_owned()
    } else {
        format!("{band} · {}", security(d.security))
    };

    // Identity on one line, the reading under it: a side-by-side head
    // squeezed the name into wrapping at the default window width.
    let head = column![
        row![
            text(d.ssid.clone())
                .font(font::UI_MEDIUM)
                .size(size::CARD_TITLE)
                .style(theme::text_primary),
            Space::new().width(Length::Fill),
            tertiary(&facts),
        ]
        .align_y(Alignment::Center),
        micro_label("signal"),
        big_value(&d.signal.to_string(), "%", true),
        meter_bar(space::HERO_METER_W, f32::from(d.signal) / 100.0, color::ACCENT),
    ]
    .spacing(space::ROW_Y);

    let readings = grid(
        vec![
            ("down", rate(d.rx_rate)),
            ("up", rate(d.tx_rate)),
            (
                "link",
                d.link_speed_mbps
                    .map_or_else(|| "—".into(), |s| format!("{s} Mbps")),
            ),
        ],
        3,
    );

    let addresses = grid(
        vec![
            ("ipv4", joined(&d.ipv4)),
            ("gateway", d.gateway.clone().unwrap_or_else(|| "—".into())),
            ("ipv6", joined(&d.ipv6)),
            ("dns", joined(&d.dns)),
            ("mac", d.mac.clone()),
            ("interface", d.interface.clone()),
        ],
        2,
    );

    panel(column![head, hairline(), readings, hairline(), addresses].spacing(space::ROW_Y)).into()
}

fn saved_list(net: &Net) -> Element<'_, Message, Theme> {
    let rows: Vec<Element<'_, Message, Theme>> = match &net.saved {
        None => vec![list_row("reading", tertiary("…"))],
        Some(s) if s.is_empty() => vec![list_row("none saved", tertiary("—"))],
        Some(s) => s
            .iter()
            .map(|ssid| list_row(ssid, pill("Forget", false, Message::ForgetWifi(ssid.clone()))))
            .collect(),
    };
    inset_list("saved networks", rows)
}

fn device_list(net: &Net) -> Element<'_, Message, Theme> {
    let paired: Vec<&BtDevice> = net.devices.iter().flatten().filter(|d| d.paired).collect();
    let rows: Vec<Element<'_, Message, Theme>> = match &net.devices {
        None => vec![list_row("reading", tertiary("…"))],
        Some(_) if paired.is_empty() => vec![list_row("none paired", tertiary("—"))],
        Some(_) => paired
            .into_iter()
            .map(|d| {
                let name = if d.name.is_empty() { &d.addr } else { &d.name };
                list_row(
                    name,
                    row![
                        tertiary(if d.connected { "connected" } else { "paired" }),
                        pill("Forget", false, Message::ForgetDevice(d.addr.clone())),
                    ]
                    .spacing(space::CONTROL_GAP)
                    .align_y(Alignment::Center),
                )
            })
            .collect(),
    };
    inset_list("bluetooth devices", rows)
}

pub fn blocks(net: &Net) -> Vec<Element<'_, Message, Theme>> {
    let mut out = vec![
        hero(net),
        row![saved_list(net), device_list(net)]
            .spacing(space::BLOCK)
            .align_y(Alignment::Start)
            .into(),
    ];
    if let Some(f) = &net.failure {
        out.push(
            text(f.clone())
                .font(font::UI)
                .size(size::BODY_SMALL)
                .style(theme::text_danger)
                .into(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_keep_a_digit_before_the_point() {
        assert_eq!(rate(0), "0 B/s");
        assert_eq!(rate(999), "999 B/s");
        assert_eq!(rate(1500), "1.5 KB/s");
        assert_eq!(rate(12_300_000), "12.3 MB/s");
    }

    #[test]
    fn the_chip_says_why_there_is_nothing() {
        let mut n = Net::default();
        assert_eq!(n.chip().0, "reading");
        n.apply(Event::WifiDetail(None));
        assert_eq!(n.chip().0, "no wifi link");
        n.down("no system bus".into());
        assert_eq!(n.chip(), ("no service".into(), "no system bus".into()));
    }
}
