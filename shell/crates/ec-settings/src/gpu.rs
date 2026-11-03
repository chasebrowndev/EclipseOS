// SPDX-License-Identifier: AGPL-3.0-only
//! The graphics half of the Rendering page: `misc.gpu-mode`'s caption and
//! `misc.discrete-gpu-apps`, the apps that always run on the discrete GPU.
//!
//! The mode itself is the schema's three-way enum, drawn by `schema_page` as
//! pills like any other; this module only supplies what the schema cannot say
//! (what each mode means, that it waits for the next login) and the list
//! editor. The list reuses [`crate::pinned`]'s pure edit functions, since it
//! is the same shape: a string list of desktop ids, written whole. Both
//! controls exist only where a discrete GPU does (`App::discrete_gpu`).

use iced::widget::{column, pick_list, row, text, Column};
use iced::{Alignment, Element, Theme};
use serde_json::json;

use ec_ui::theme;
use ec_ui::tokens::{font, size, space};
use ec_ui::widget::{glass_panel as panel, hairline, list_row, list_row_at, micro_label, pill};

use crate::app::{row_id, App, Message};
use crate::pinned::{addable, edited, list_of, name_of, Choice, Msg};

pub const MODE: &str = "misc.gpu-mode";
pub const APPS: &str = "misc.discrete-gpu-apps";

/// What a graphics mode does, in a line.
pub fn mode_blurb(mode: &str) -> Option<&'static str> {
    Some(match mode {
        "integrated" => "Built-in GPU only. Best battery; ports wired to the discrete GPU stay off.",
        "hybrid" => "Each GPU drives its own screens; apps that ask for it run on the discrete GPU.",
        "dedicated" => "The discrete GPU renders everything. Best performance, most power.",
        _ => return None,
    })
}

pub const LOGIN_NOTE: &str = "Takes effect at next login";

/// Whether the mode is hybrid; an unset key is the default, which is hybrid.
pub fn is_hybrid(app: &App) -> bool {
    app.key(MODE)
        .is_none_or(|k| k.value.as_str().is_none_or(|m| m == "hybrid"))
}

pub fn update(app: &mut App, msg: Msg) {
    let list = app.key(APPS).map(|k| list_of(&k.value)).unwrap_or_default();
    if let Some(next) = edited(&list, &msg) {
        app.write(APPS, json!(next));
    }
}

fn tertiary<'a>(t: &str) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_tertiary)
        .into()
}

/// The "always on <GPU>" list: one hairlined row per app, then the add
/// control. Shown only under hybrid, the one mode where the list decides
/// anything.
pub fn view(app: &App) -> Option<Element<'_, Message, Theme>> {
    let gpu = app.discrete_gpu.as_deref()?;
    if !is_hybrid(app) || app.key(APPS).is_none() {
        return None;
    }
    let list = app.key(APPS).map(|k| list_of(&k.value)).unwrap_or_default();
    let mut body = Column::new().push(
        iced::widget::container(
            row![
                micro_label("always on the discrete gpu"),
                tertiary(&format!("these apps run on {gpu}"))
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
        )
        .padding([space::ROW_Y, space::CARD]),
    );
    if list.is_empty() {
        body = body.push(hairline()).push(list_row("none chosen", tertiary("—")));
    }
    for (i, id) in list.iter().enumerate() {
        let name = name_of(&app.installed, id);
        let remove = pill("Remove", false, Message::GpuApps(Msg::Remove(i)));
        let value: Element<'_, Message, Theme> = match name {
            Some(_) => remove,
            None => row![tertiary("not installed"), remove]
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
        Message::GpuApps(Msg::Add(c.id))
    })
    .placeholder("Add an app");
    body = body.push(hairline()).push(list_row("Add", add));
    Some(
        iced::widget::container(column![panel(app.glass_radius, app.hairline_cards, body)])
            .id(row_id(APPS))
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_a_line() {
        for m in ["integrated", "hybrid", "dedicated"] {
            assert!(mode_blurb(m).is_some());
        }
        assert!(mode_blurb("other").is_none());
    }
}
