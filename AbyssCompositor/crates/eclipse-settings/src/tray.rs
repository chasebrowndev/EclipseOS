// SPDX-License-Identifier: AGPL-3.0-only
//! Where each tray entry lives: `bar.tray.pinned` and `bar.tray.hidden` read
//! as three lanes, and every move written back as those two lists.
//!
//! The rules are the taskbar's (`hyperion/src/view.rs::tray_entries`), not
//! this app's: hidden wins over pinned, anything neither pinned nor hidden is
//! in the overflow drawer, and an unset `pinned` means the built-in order.
//!
//! The live status-notifier items come from [`feed`], which hosts against
//! whichever process serves the tray watcher (normally the taskbar) while the
//! Taskbar pane is showing. Only their ids cross over: nothing an app puts in
//! its item is shown or logged here.

use std::time::Duration;

use iced::Subscription;
use serde_json::Value;

use crate::app::Message;

/// The entries the taskbar draws itself. Status-notifier items come and go
/// with their apps; these are always nameable.
pub const BUILTINS: [&str; 4] = ["network", "bluetooth", "battery", "volume"];

/// What the taskbar pins when `bar.tray.pinned` is unset. Mirrors
/// `DEFAULT_PINNED` in the taskbar; if the two drift, this pane shows a
/// layout the bar is not drawing.
pub const DEFAULT_PINNED: [&str; 2] = ["network", "battery"];

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
    v.as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect())
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
        match &self.pinned {
            Some(p) => p.clone(),
            None => DEFAULT_PINNED.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// Every id there is anything to say about: the built-ins, then the live
    /// items, then whatever else the two lists name (an app that is not
    /// running keeps its place), first mention first.
    pub fn ids(&self) -> Vec<String> {
        let mut all: Vec<String> = BUILTINS.iter().map(|s| (*s).to_owned()).collect();
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
    /// `hidden`. Touching `pinned` at all makes it explicit — the built-in
    /// order is written out, so it stops being "unset".
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
    fn unset_is_the_built_in_order_and_empty_pins_nothing() {
        let t = tray(Value::Null, json!([]));
        assert_eq!(t.taskbar(), ["network", "battery"]);
        assert_eq!(t.in_lane(Lane::Overflow), ["bluetooth", "volume"]);
        let t = tray(json!([]), json!([]));
        assert!(t.taskbar().is_empty());
        assert_eq!(t.in_lane(Lane::Overflow).len(), BUILTINS.len());
    }

    #[test]
    fn hidden_wins_over_pinned() {
        let t = tray(json!(["network", "volume"]), json!(["network"]));
        assert_eq!(t.lane_of("network"), Lane::Hidden);
        assert_eq!(t.taskbar(), ["volume"]);
    }

    #[test]
    fn ids_named_only_in_config_are_listed() {
        let t = tray(json!(["org.syncthing"]), json!(["nm-applet"]));
        let ids = t.ids();
        assert!(ids.contains(&"org.syncthing".to_owned()));
        assert!(ids.contains(&"nm-applet".to_owned()));
    }

    #[test]
    fn live_items_follow_the_built_ins_and_are_listed_once() {
        let mut t = tray(json!(["org.syncthing", "network"]), json!(["nm-applet"]));
        t.live = live_ids(
            ["spotify", "org.syncthing", "spotify", ""]
                .map(str::to_owned)
                .into_iter(),
        );
        assert_eq!(t.live, ["spotify", "org.syncthing"]);
        assert_eq!(
            t.ids(),
            [
                "network",
                "bluetooth",
                "battery",
                "volume",
                "spotify",
                "org.syncthing",
                "nm-applet"
            ]
        );
        assert_eq!(t.lane_of("spotify"), Lane::Overflow);
        assert_eq!(
            t.in_lane(Lane::Overflow),
            ["bluetooth", "battery", "volume", "spotify"]
        );
    }

    #[test]
    fn a_move_writes_only_what_changed() {
        let t = tray(json!(["network"]), json!([]));
        let w = t.moved("volume", Lane::Hidden);
        assert_eq!(w.pinned, None);
        assert_eq!(w.hidden, Some(vec!["volume".to_owned()]));

        let w = t.moved("network", Lane::Overflow);
        assert_eq!(w.pinned, Some(vec![]));
        assert_eq!(w.hidden, None);
    }

    #[test]
    fn touching_pinned_makes_the_default_explicit() {
        let t = tray(Value::Null, json!([]));
        let w = t.moved("volume", Lane::Taskbar);
        assert_eq!(
            w.pinned,
            Some(vec!["network".into(), "battery".into(), "volume".into()])
        );
    }

    #[test]
    fn unhiding_to_the_taskbar_clears_hidden_and_pins() {
        let t = tray(json!([]), json!(["volume"]));
        let w = t.moved("volume", Lane::Taskbar);
        assert_eq!(w.pinned, Some(vec!["volume".to_owned()]));
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
        assert_eq!(t.shifted("volume", true), None);
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
