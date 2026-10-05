// SPDX-License-Identifier: AGPL-3.0-only
//! `bar.pinned-apps` (ADR 0074): the apps pinned to the taskbar, as a list
//! that is added to from the installed apps, removed from and reordered.
//!
//! `set_config_value` writes a whole string list as one value, so every edit
//! here is the new list, computed by the pure functions below and written
//! through [`App::write`]; nothing is kept beside what the compositor says.
//! An id with no desktop entry stays in the list and reads as "not
//! installed": the taskbar skips it, and the app may come back.

use iced::widget::{column, pick_list, row, text, Column};
use iced::{Alignment, Element, Theme};
use serde_json::{json, Value};

use ec_services::apps::Entry;
use ec_ui::theme;
use ec_ui::tokens::{font, size, space};
use ec_ui::widget::{hairline, list_row, list_row_at, micro_label, panel, pill};

use crate::app::{row_id, App, Message};

pub const PATH: &str = "bar.pinned-apps";

/// The pane's messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Add(String),
    Remove(usize),
    Up(usize),
    Down(usize),
}

/// An installed app the list can take: its entry id (no `.desktop`) and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub name: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// The desktop entries that can be pinned, by name. Read once, when the app
/// opens: the same scanner the launcher and the taskbar use.
pub fn installed() -> Vec<Choice> {
    #[cfg(not(test))]
    let entries = ec_services::apps::scan(None);
    #[cfg(test)]
    let entries: Vec<Entry> = Vec::new();
    choices(&entries)
}

fn choices(entries: &[Entry]) -> Vec<Choice> {
    entries
        .iter()
        .filter(|e| !ec_services::apps::is_path_binary(e))
        .map(|e| Choice {
            id: e.id.strip_suffix(".desktop").unwrap_or(&e.id).to_owned(),
            name: e.name.clone(),
        })
        .collect()
}

/// The list as `get_config` reported it.
pub fn list_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The name a list id is shown under, or `None` when no entry is installed.
pub fn name_of<'a>(installed: &'a [Choice], id: &str) -> Option<&'a str> {
    installed
        .iter()
        .find(|c| same(&c.id, id))
        .map(|c| c.name.as_str())
}

/// Installed apps not yet in the list, for the add control.
pub fn addable<'a>(installed: &'a [Choice], list: &[String]) -> Vec<&'a Choice> {
    installed
        .iter()
        .filter(|c| !list.iter().any(|id| same(id, &c.id)))
        .collect()
}

/// `list` after `msg`, or `None` when it changes nothing (a move off either
/// end, an id already pinned, an index that is not there).
pub fn edited(list: &[String], msg: &Msg) -> Option<Vec<String>> {
    let mut next = list.to_vec();
    match msg {
        Msg::Add(id) => {
            if list.iter().any(|p| same(p, id)) {
                return None;
            }
            next.push(id.clone());
        }
        Msg::Remove(i) => {
            if *i >= next.len() {
                return None;
            }
            next.remove(*i);
        }
        Msg::Up(i) => {
            if *i == 0 || *i >= next.len() {
                return None;
            }
            next.swap(*i, *i - 1);
        }
        Msg::Down(i) => {
            if i + 1 >= next.len() {
                return None;
            }
            next.swap(*i, *i + 1);
        }
    }
    Some(next)
}

pub fn update(app: &mut App, msg: Msg) {
    let list = app.key(PATH).map(|k| list_of(&k.value)).unwrap_or_default();
    if let Some(next) = edited(&list, &msg) {
        app.write(PATH, json!(next));
    }
}

fn tertiary<'a>(t: &str) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_tertiary)
        .into()
}

/// The pinned-apps block: one hairlined row per pin, then the add control.
pub fn view(app: &App) -> Element<'_, Message, Theme> {
    let list = app.key(PATH).map(|k| list_of(&k.value)).unwrap_or_default();
    let mut body = Column::new().push(
        iced::widget::container(
            row![
                micro_label("pinned apps"),
                tertiary("launch from the taskbar, in this order")
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
        )
        .padding([space::ROW_Y, space::CARD]),
    );
    if list.is_empty() {
        body = body.push(hairline()).push(list_row("none pinned", tertiary("—")));
    }
    for (i, id) in list.iter().enumerate() {
        let name = name_of(&app.installed, id);
        let controls = row![
            pill("Up", false, Message::Pinned(Msg::Up(i))),
            pill("Down", false, Message::Pinned(Msg::Down(i))),
            pill("Remove", false, Message::Pinned(Msg::Remove(i))),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
        let value: Element<'_, Message, Theme> = match name {
            Some(_) => controls.into(),
            None => row![tertiary("not installed"), controls]
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center)
                .into(),
        };
        body = body
            .push(hairline())
            .push(list_row_at(name.unwrap_or(id), value, name.is_none()));
    }
    let options: Vec<Choice> = addable(&app.installed, &list).into_iter().cloned().collect();
    let add = pick_list(options, None::<Choice>, |c: Choice| {
        Message::Pinned(Msg::Add(c.id))
    })
    .placeholder("Add an app");
    body = body.push(hairline()).push(list_row("Add", add));
    iced::widget::container(column![panel(app.glass_radius, body)])
        .id(row_id(PATH))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn add_appends_once() {
        let l = ids(&["a"]);
        assert_eq!(edited(&l, &Msg::Add("b".into())), Some(ids(&["a", "b"])));
        assert_eq!(edited(&l, &Msg::Add("A".into())), None);
    }

    #[test]
    fn remove_and_moves_stay_in_bounds() {
        let l = ids(&["a", "b", "c"]);
        assert_eq!(edited(&l, &Msg::Remove(1)), Some(ids(&["a", "c"])));
        assert_eq!(edited(&l, &Msg::Remove(3)), None);
        assert_eq!(edited(&l, &Msg::Up(1)), Some(ids(&["b", "a", "c"])));
        assert_eq!(edited(&l, &Msg::Up(0)), None);
        assert_eq!(edited(&l, &Msg::Down(1)), Some(ids(&["a", "c", "b"])));
        assert_eq!(edited(&l, &Msg::Down(2)), None);
    }

    #[test]
    fn a_missing_entry_keeps_its_place_and_is_not_offered_again() {
        let installed = vec![
            Choice {
                id: "firefox".into(),
                name: "Firefox".into(),
            },
            Choice {
                id: "foot".into(),
                name: "Foot".into(),
            },
        ];
        let l = ids(&["gone", "firefox"]);
        assert_eq!(name_of(&installed, "gone"), None);
        assert_eq!(name_of(&installed, "Firefox"), Some("Firefox"));
        let left: Vec<&str> = addable(&installed, &l).iter().map(|c| c.id.as_str()).collect();
        assert_eq!(left, ["foot"]);
    }

    #[test]
    fn the_list_reads_strings_and_ignores_the_rest() {
        assert_eq!(list_of(&json!(["a", 3, "b"])), ids(&["a", "b"]));
        assert!(list_of(&Value::Null).is_empty());
    }
}
