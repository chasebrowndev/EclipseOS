// SPDX-License-Identifier: AGPL-3.0-only
//! Pinned apps on the strip (ADR 0074): the pure half.
//!
//! `bar.pinned-apps` is an ordered list of desktop-entry ids. Each id that
//! resolves to an entry yields one chip. A running window of the app merges
//! into that chip; with none, the chip is *idle* — a synthetic [`Window`] the
//! rest of the bar (solver, motion, popups) treats as a first-class cell, told
//! apart by [`is_idle`]. Nothing here touches the compositor or the disk.

use ec_services::apps::Entry;

use crate::model::{Trust, Window};

/// Set on the handle of every idle chip. A compositor handle is a small
/// counter, so the bit cannot collide with one; it is what keeps an idle chip
/// from ever being sent to the compositor as if it were a window.
const IDLE: u64 = 1 << 63;

/// One pinned app whose desktop entry exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pinned {
    /// The list id: the entry's basename, no `.desktop`.
    pub id: String,
    /// What the chip draws when the app has no window.
    pub idle: Window,
}

impl Pinned {
    pub fn new(id: &str, name: &str) -> Self {
        Pinned {
            id: id.to_owned(),
            idle: Window {
                handle: IDLE | (fnv(id) & !IDLE),
                // The id doubles as the icon key: `icons::resolve` finds the
                // entry's `Icon=` from it, as it does for a window's app_id.
                app_id: id.to_owned(),
                title: name.to_owned(),
                workspace: None,
                output: None,
                focused: false,
                // The minimized-chip treatment: dimmed, no fill.
                minimized: true,
                pid: None,
                program: None,
                trust: Trust::Private,
            },
        }
    }
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Whether `w` is an idle pinned chip rather than a compositor window.
pub fn is_idle(w: &Window) -> bool {
    w.handle & IDLE != 0
}

/// The list id an entry is pinned under: `org.x.Thing.desktop` -> `org.x.Thing`.
pub fn id_of(entry: &Entry) -> &str {
    entry.id.strip_suffix(".desktop").unwrap_or(&entry.id)
}

/// The same match the "Open in new window" action makes: the app id is the
/// entry's basename, compared without regard to case.
pub fn matches(id: &str, app_id: &str) -> bool {
    !app_id.is_empty() && id.eq_ignore_ascii_case(app_id)
}

/// The entry `id` names, if one is installed.
pub fn entry_for<'a>(entries: &'a [Entry], id: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| matches(id, id_of(e)))
}

/// The configured ids that resolve to an installed entry, in list order. An
/// unknown id is skipped here and stays in the list (the app may come back).
pub fn resolve(ids: &[String], entries: &[Entry]) -> Vec<Pinned> {
    let mut out: Vec<Pinned> = Vec::new();
    for id in ids {
        if out.iter().any(|p| p.id.eq_ignore_ascii_case(id)) {
            continue;
        }
        if let Some(entry) = entry_for(entries, id) {
            out.push(Pinned::new(id, &entry.name));
        }
    }
    out
}

/// The strip's order: one chip per pin, in list order — the first running
/// window of the app if there is one, else the idle chip — then every window
/// that did not merge, in the order it arrived.
pub fn order<'a>(pins: &'a [Pinned], windows: Vec<&'a Window>) -> Vec<&'a Window> {
    let mut taken = vec![false; windows.len()];
    let mut out: Vec<&Window> = Vec::with_capacity(pins.len() + windows.len());
    for pin in pins {
        let hit = windows
            .iter()
            .enumerate()
            .find(|(i, w)| !taken[*i] && matches(&pin.id, &w.app_id));
        match hit {
            Some((i, w)) => {
                taken[i] = true;
                out.push(w);
            }
            None => out.push(&pin.idle),
        }
    }
    out.extend(
        windows
            .iter()
            .enumerate()
            .filter(|(i, _)| !taken[*i])
            .map(|(_, w)| *w),
    );
    out
}

/// The listed id whose chip `w` is (or merged into), if any.
pub fn id_for<'a>(ids: &'a [String], w: &Window) -> Option<&'a str> {
    ids.iter().map(String::as_str).find(|id| matches(id, &w.app_id))
}

/// `list` with `id` appended. `None` when it is already pinned.
pub fn pinned(list: &[String], id: &str) -> Option<Vec<String>> {
    if list.iter().any(|p| p.eq_ignore_ascii_case(id)) {
        return None;
    }
    let mut next = list.to_vec();
    next.push(id.to_owned());
    Some(next)
}

/// `list` without `id`. `None` when it was not there.
pub fn unpinned(list: &[String], id: &str) -> Option<Vec<String>> {
    let next: Vec<String> = list
        .iter()
        .filter(|p| !p.eq_ignore_ascii_case(id))
        .cloned()
        .collect();
    (next.len() != list.len()).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str) -> Entry {
        Entry {
            id: id.to_owned(),
            name: name.to_owned(),
            comment: None,
            argv: vec![name.to_lowercase()],
            terminal: false,
            keywords: Vec::new(),
        }
    }

    fn win(handle: u64, app_id: &str) -> Window {
        let mut w = Pinned::new("x", "t").idle;
        w.handle = handle;
        w.app_id = app_id.to_owned();
        w.minimized = false;
        w
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn an_idle_chip_can_never_be_mistaken_for_a_window() {
        let p = Pinned::new("firefox", "Firefox");
        assert!(is_idle(&p.idle));
        assert!(!is_idle(&win(7, "firefox")));
        // Stable: motion keys the chip by it.
        assert_eq!(p.idle.handle, Pinned::new("firefox", "Firefox").idle.handle);
        assert_ne!(p.idle.handle, Pinned::new("foot", "Foot").idle.handle);
    }

    #[test]
    fn unknown_ids_are_skipped_and_duplicates_collapse() {
        let entries = [
            entry("firefox.desktop", "Firefox"),
            entry("org.x.Thing.desktop", "Thing"),
        ];
        let got = resolve(&ids(&["gone", "org.x.Thing", "Firefox", "firefox"]), &entries);
        let names: Vec<&str> = got.iter().map(|p| p.idle.title.as_str()).collect();
        assert_eq!(names, ["Thing", "Firefox"]);
    }

    #[test]
    fn a_running_window_merges_into_its_pin_and_pins_lead_in_list_order() {
        let pins = resolve(
            &ids(&["steam", "firefox", "foot"]),
            &[
                entry("steam.desktop", "Steam"),
                entry("firefox.desktop", "Firefox"),
                entry("foot.desktop", "Foot"),
            ],
        );
        let (a, b, c, d) = (win(1, "kitty"), win(2, "foot"), win(3, "Firefox"), win(4, "foot"));
        let strip = order(&pins, vec![&a, &b, &c, &d]);
        let got: Vec<u64> = strip.iter().map(|w| w.handle).collect();
        // steam idle, firefox -> 3, foot -> its first window (2), then the
        // rest in arrival order: kitty and the second foot.
        assert!(is_idle(strip[0]));
        assert_eq!(&got[1..], [3, 2, 1, 4]);
    }

    #[test]
    fn with_no_pins_the_strip_is_untouched() {
        let (a, b) = (win(1, "foot"), win(2, "kitty"));
        let strip = order(&[], vec![&a, &b]);
        assert_eq!(strip.iter().map(|w| w.handle).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn list_edits_append_remove_and_refuse_noops() {
        let list = ids(&["a", "b"]);
        assert_eq!(pinned(&list, "c"), Some(ids(&["a", "b", "c"])));
        assert_eq!(pinned(&list, "A"), None);
        assert_eq!(unpinned(&list, "a"), Some(ids(&["b"])));
        assert_eq!(unpinned(&list, "z"), None);
    }

    #[test]
    fn a_window_finds_the_pin_it_belongs_to() {
        let list = ids(&["firefox"]);
        assert_eq!(id_for(&list, &win(1, "Firefox")), Some("firefox"));
        assert_eq!(id_for(&list, &win(2, "kitty")), None);
        assert_eq!(id_for(&list, &win(3, "")), None);
    }
}
