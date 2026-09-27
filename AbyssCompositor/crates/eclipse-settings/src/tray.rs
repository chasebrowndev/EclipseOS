// SPDX-License-Identifier: AGPL-3.0-only
//! Where each tray entry lives: `bar.tray.pinned` and `bar.tray.hidden` read
//! as three lanes, and every move written back as those two lists.
//!
//! The rules are the taskbar's (`hyperion/src/view.rs::tray_entries`), not
//! this app's: hidden wins over pinned, anything neither pinned nor hidden is
//! in the overflow drawer, and an unset `pinned` pins nothing.
//!
//! The applets the bar draws itself are widgets now (ADR 0065) and are
//! ordered in the widget lane; their old ids in these two lists are ignored
//! by the taskbar, so the lanes leave them out too.
//!
//! The live status-notifier items come from [`feed`], which hosts against
//! whichever process serves the tray watcher (normally the taskbar) while the
//! Taskbar pane is showing. Only their ids cross over: nothing an app puts in
//! its item is shown or logged here.

use std::time::Duration;

use iced::Subscription;
use serde_json::Value;

use crate::app::Message;

/// Built-in applet ids these lists carried before the applets became
/// widgets. Still accepted by the compositor, with a deprecation warning, and
/// ignored by the taskbar.
pub const LEGACY: [&str; 4] = ["network", "bluetooth", "battery", "volume"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Taskbar,
    Overflow,
    Hidden,
}

/// The two lists as the compositor reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tray {
    /// `None` is unset: the built-in order.
    pub pinned: Option<Vec<String>>,
    pub hidden: Vec<String>,
    /// Ids of the status-notifier items running now, first seen first.
    pub live: Vec<String>,
}

/// A move, as the writes it needs. `None` leaves that key alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Writes {
    pub pinned: Option<Vec<String>>,
    pub hidden: Option<Vec<String>>,
}

fn strings(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .filter(|s| !LEGACY.contains(s))
            .map(str::to_owned)
            .collect()
    })
}

impl Tray {
    pub fn from_values(pinned: &Value, hidden: &Value) -> Self {
        Self {
            pinned: strings(pinned),
            hidden: strings(hidden).unwrap_or_default(),
            live: Vec::new(),
        }
    }

    /// The pinned order in effect, unset resolved.
    fn wanted(&self) -> Vec<String> {
        self.pinned.clone().unwrap_or_default()
    }

    /// Every id there is anything to say about: the live items, then
    /// whatever else the two lists name (an app that is not running keeps its
    /// place), first mention first.
    pub fn ids(&self) -> Vec<String> {
        let mut all: Vec<String> = Vec::new();
        let named = self.wanted().into_iter().chain(self.hidden.iter().cloned());
        for id in self.live.iter().cloned().chain(named) {
            if !all.contains(&id) {
                all.push(id);
            }
        }
        all
    }

    pub fn lane_of(&self, id: &str) -> Lane {
        if self.hidden.iter().any(|h| h == id) {
            Lane::Hidden
        } else if self.wanted().iter().any(|p| p == id) {
            Lane::Taskbar
        } else {
            Lane::Overflow
        }
    }

    /// The taskbar lane, in pinned order.
    pub fn taskbar(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for id in self.wanted() {
            if self.lane_of(&id) == Lane::Taskbar && !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    pub fn in_lane(&self, lane: Lane) -> Vec<String> {
        match lane {
            Lane::Taskbar => self.taskbar(),
            _ => self
                .ids()
                .into_iter()
                .filter(|id| self.lane_of(id) == lane)
                .collect(),
        }
    }

    /// Send `id` to `to`. Pinning appends to the end of the taskbar; the
    /// other two take it out of `pinned`, and only hiding puts it in
    /// `hidden`. Touching `pinned` at all makes it explicit.
    pub fn moved(&self, id: &str, to: Lane) -> Writes {
        let mut pinned = self.wanted();
        let mut hidden = self.hidden.clone();
        pinned.retain(|p| p != id);
        hidden.retain(|h| h != id);
        match to {
            Lane::Taskbar => pinned.push(id.to_owned()),
            Lane::Overflow => {}
            Lane::Hidden => hidden.push(id.to_owned()),
        }
        Writes {
            pinned: (Some(&pinned) != self.pinned.as_ref()).then_some(pinned),
            hidden: (hidden != self.hidden).then_some(hidden),
        }
    }

    /// Move a pinned `id` one place earlier (`-1`) or later (`+1`) in the
    /// taskbar. `None` when it is not pinned or is already at that end.
    pub fn shifted(&self, id: &str, later: bool) -> Option<Vec<String>> {
        let order = self.taskbar();
        let at = order.iter().position(|p| p == id)?;
        let to = if later { at + 1 } else { at.checked_sub(1)? };
        let other = order.get(to)?;
        // Swap in the full list, so an entry that is pinned but currently
        // hidden keeps its place for when it comes back.
        let mut pinned = self.wanted();
        let a = pinned.iter().position(|p| p == id)?;
        let b = pinned.iter().position(|p| p == other)?;
        pinned.swap(a, b);
        Some(pinned)
    }
}

/// How often the feed thread looks up from the tray to see if the pane is gone.
const TICK: Duration = Duration::from_millis(250);

/// The live tray ids, as [`Message::TrayLive`], for as long as the
/// subscription lives. `None` once means the session bus is unreachable.
///
/// `tray::observe` only reads: it never takes the watcher name, so the tray
/// stays the taskbar's even if the taskbar restarts while this pane is open.
/// Dropping the updates when the pane closes ends its thread.
pub fn feed() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut sender| {
            std::thread::spawn(move || {
                let Ok(updates) = eclipse_services::tray::observe() else {
                    let _ = sender.try_send(Message::TrayLive(None));
                    return;
                };
                loop {
                    let alive = match updates.recv_timeout(TICK) {
                        Some(eclipse_services::tray::TrayUpdate::Items(items)) => {
                            let ids = live_ids(items.into_iter().map(|i| i.id));
                            match sender.try_send(Message::TrayLive(Some(ids))) {
                                Ok(()) => true,
                                Err(e) => !e.is_disconnected(),
                            }
                        }
                        Some(_) => true,
                        None => !sender.is_closed(),
                    };
                    if !alive {
                        return;
                    }
                }
            });
        })
    })
}

/// One id per app, first instance first. Two windows of one app are one
/// entry in the bar's lists, so they are one chip here.
fn live_ids(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if !id.is_empty() && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tray(pinned: Value, hidden: Value) -> Tray {
        Tray::from_values(&pinned, &hidden)
    }

    #[test]
    fn unset_and_empty_both_pin_nothing() {
        for pinned in [Value::Null, json!([])] {
            let mut t = tray(pinned, json!([]));
            t.live = vec!["spotify".into()];
            assert!(t.taskbar().is_empty());
            assert_eq!(t.in_lane(Lane::Overflow), ["spotify"]);
        }
    }

    #[test]
    fn the_old_applet_ids_are_not_tray_entries() {
        let t = tray(json!(["network", "org.syncthing"]), json!(["volume"]));
        assert_eq!(t.taskbar(), ["org.syncthing"]);
        assert!(t.ids().iter().all(|id| !LEGACY.contains(&id.as_str())));
    }

    #[test]
    fn hidden_wins_over_pinned() {
        let t = tray(json!(["nm-applet", "spotify"]), json!(["nm-applet"]));
        assert_eq!(t.lane_of("nm-applet"), Lane::Hidden);
        assert_eq!(t.taskbar(), ["spotify"]);
    }

    #[test]
    fn ids_named_only_in_config_are_listed() {
        let t = tray(json!(["org.syncthing"]), json!(["nm-applet"]));
        let ids = t.ids();
        assert!(ids.contains(&"org.syncthing".to_owned()));
        assert!(ids.contains(&"nm-applet".to_owned()));
    }

    #[test]
    fn live_items_come_first_and_are_listed_once() {
        let mut t = tray(json!(["org.syncthing", "steam"]), json!(["nm-applet"]));
        t.live = live_ids(
            ["spotify", "org.syncthing", "spotify", ""]
                .map(str::to_owned)
                .into_iter(),
        );
        assert_eq!(t.live, ["spotify", "org.syncthing"]);
        assert_eq!(t.ids(), ["spotify", "org.syncthing", "steam", "nm-applet"]);
        assert_eq!(t.lane_of("spotify"), Lane::Overflow);
        assert_eq!(t.in_lane(Lane::Overflow), ["spotify"]);
    }

    #[test]
    fn a_move_writes_only_what_changed() {
        let t = tray(json!(["steam"]), json!([]));
        let w = t.moved("spotify", Lane::Hidden);
        assert_eq!(w.pinned, None);
        assert_eq!(w.hidden, Some(vec!["spotify".to_owned()]));

        let w = t.moved("steam", Lane::Overflow);
        assert_eq!(w.pinned, Some(vec![]));
        assert_eq!(w.hidden, None);
    }

    #[test]
    fn touching_pinned_makes_it_explicit() {
        let t = tray(Value::Null, json!([]));
        let w = t.moved("spotify", Lane::Taskbar);
        assert_eq!(w.pinned, Some(vec!["spotify".to_owned()]));
    }

    #[test]
    fn unhiding_to_the_taskbar_clears_hidden_and_pins() {
        let t = tray(json!([]), json!(["spotify"]));
        let w = t.moved("spotify", Lane::Taskbar);
        assert_eq!(w.pinned, Some(vec!["spotify".to_owned()]));
        assert_eq!(w.hidden, Some(vec![]));
    }

    #[test]
    fn shifting_stops_at_the_ends() {
        let t = tray(json!(["a", "b", "c"]), json!([]));
        assert_eq!(
            t.shifted("b", false),
            Some(vec!["b".into(), "a".into(), "c".into()])
        );
        assert_eq!(t.shifted("a", false), None);
        assert_eq!(t.shifted("c", true), None);
        assert_eq!(t.shifted("spotify", true), None);
    }

    #[test]
    fn shifting_skips_a_hidden_neighbour() {
        let t = tray(json!(["a", "b", "c"]), json!(["b"]));
        assert_eq!(
            t.shifted("a", true),
            Some(vec!["c".into(), "b".into(), "a".into()])
        );
    }
}
