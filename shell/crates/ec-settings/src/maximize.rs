// SPDX-License-Identifier: AGPL-3.0-only
//! `general.maximize-first-window` and its exemptions
//! (`general.maximize-first-window-exempt`): the toggle, and the apps that
//! never open maximized.
//!
//! The list is a string list of window `app_id`s, written whole through
//! [`App::write`], so the add/remove arithmetic is [`crate::pinned`]'s. What
//! differs is the id: a window is matched on its `app_id` (the desktop
//! entry's `StartupWMClass` when it has one, else its id lowercased), which
//! is not always the desktop id the pinned list stores. An id with no entry
//! stays in the list and shows as the raw `app_id`.
//!
//! Both keys only act under `mode "de"`; the block is drawn in every mode
//! and says so in a line.

use iced::widget::{column, pick_list, row, text, Column};
use iced::{Alignment, Element, Theme};
use serde_json::json;

use ec_services::apps::Entry;
use ec_ui::theme;
use ec_ui::tokens::{font, size, space};
use ec_ui::widget::{dimmed_at, glass_panel as panel, hairline, list_row_at, micro_label, pill, row_caption};

use crate::app::{row_id, setting_row_at, App, Message};
use crate::pinned::{addable, edited, list_of, name_of, Choice, Msg};

pub const TOGGLE: &str = "general.maximize-first-window";
pub const EXEMPT: &str = "general.maximize-first-window-exempt";

/// The line under the toggle.
pub const HINT: &str = "Applies in Desktop mode.";

/// The `app_id` a window of this entry reports: `StartupWMClass` if the file
/// has one, else the desktop id without `.desktop`, lowercased.
pub fn app_id(entry: &Entry, startup_wm_class: Option<&str>) -> String {
    match startup_wm_class.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => c.to_owned(),
        None => entry
            .id
            .strip_suffix(".desktop")
            .unwrap_or(&entry.id)
            .to_lowercase(),
    }
}

/// `StartupWMClass` from the `[Desktop Entry]` group of a file's text.
pub fn wm_class(text: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry {
            if let Some(v) = line.strip_prefix("StartupWMClass=") {
                return Some(v.trim().to_owned());
            }
        }
    }
    None
}

/// The installed apps, named, under the `app_id` their windows carry. Read
/// once when the app opens, with the same scanner the launcher uses.
pub fn installed() -> Vec<Choice> {
    #[cfg(not(test))]
    let entries = ec_services::apps::scan(None);
    #[cfg(test)]
    let entries: Vec<Entry> = Vec::new();
    let dirs = ec_services::apps::search_path();
    let mut out: Vec<Choice> = Vec::new();
    for e in entries.iter().filter(|e| !ec_services::apps::is_path_binary(e)) {
        let class = dirs
            .iter()
            .find_map(|d| std::fs::read_to_string(d.join("applications").join(&e.id)).ok())
            .and_then(|t| wm_class(&t));
        let id = app_id(e, class.as_deref());
        if !out.iter().any(|c| c.id.eq_ignore_ascii_case(&id)) {
            out.push(Choice {
                id,
                name: e.name.clone(),
            });
        }
    }
    out
}

pub fn update(app: &mut App, msg: Msg) {
    let list = app.key(EXEMPT).map(|k| list_of(&k.value)).unwrap_or_default();
    if let Some(next) = edited(&list, &msg) {
        app.write(EXEMPT, json!(next));
    }
}

fn tertiary<'a>(t: &str) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_tertiary)
        .into()
}

/// The block: the toggle, its hint, then the exemptions. `None` when the
/// compositor reports neither key (an older one).
pub fn view(app: &App) -> Option<Element<'_, Message, Theme>> {
    let toggle = app.key(TOGGLE)?;
    let on = toggle.as_bool();
    let dim = !on;
    let list = app.key(EXEMPT).map(|k| list_of(&k.value)).unwrap_or_default();

    let mut rows = Column::new().push(setting_row_at(
        app,
        "Maximize first window on a workspace",
        toggle,
        false,
    ));
    rows = rows.push(iced::widget::container(tertiary(HINT)).padding([0.0, space::CARD]));

    let mut exempt = Column::new()
        .push(iced::widget::container(hairline()).padding([0.0, space::CARD]))
        .push(row_caption(if dim {
            "Exemptions \u{b7} not in use"
        } else {
            "Exemptions"
        }))
        .spacing(space::ROW_Y);
    if list.is_empty() {
        exempt = exempt.push(list_row_at("none", tertiary("\u{2014}"), dim));
    }
    for (i, id) in list.iter().enumerate() {
        let name = name_of(&app.wm_apps, id);
        let remove = pill("Remove", false, Message::Exempt(Msg::Remove(i)));
        let value: Element<'_, Message, Theme> = match name {
            Some(_) => remove,
            None => row![tertiary("not installed"), remove]
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center)
                .into(),
        };
        exempt = exempt.push(list_row_at(name.unwrap_or(id), value, dim));
    }
    let options: Vec<Choice> = addable(&app.wm_apps, &list).into_iter().cloned().collect();
    let add = pick_list(options, None::<Choice>, |c: Choice| {
        Message::Exempt(Msg::Add(c.id))
    })
    .placeholder("Add an app");
    exempt = exempt.push(list_row_at("Add app", add, dim));
    rows = rows.push(dimmed_at(exempt, dim, app.blur));

    let body = Column::new()
        .push(iced::widget::container(micro_label("new windows")).padding([space::ROW_Y, space::CARD]))
        .push(hairline())
        .push(rows);
    Some(
        iced::widget::container(column![panel(app.glass_radius, app.hairline_cards, body)])
            .id(row_id(EXEMPT))
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> Entry {
        Entry {
            id: id.into(),
            name: "X".into(),
            comment: None,
            argv: vec!["x".into()],
            terminal: false,
            keywords: vec![],
            prefers_discrete_gpu: false,
        }
    }

    #[test]
    fn the_class_wins_else_the_lowercased_id() {
        assert_eq!(app_id(&entry("Foo.desktop"), None), "foo");
        assert_eq!(app_id(&entry("Foo.desktop"), Some("Bar")), "Bar");
        assert_eq!(app_id(&entry("Foo.desktop"), Some(" ")), "foo");
    }

    #[test]
    fn the_class_is_read_from_the_entry_group_only() {
        let t = "[Desktop Action a]\nStartupWMClass=no\n[Desktop Entry]\nName=A\nStartupWMClass=Yes\n";
        assert_eq!(wm_class(t).as_deref(), Some("Yes"));
        assert_eq!(wm_class("[Desktop Entry]\nName=A\n"), None);
    }
}
