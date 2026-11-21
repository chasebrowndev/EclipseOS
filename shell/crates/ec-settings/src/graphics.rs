// SPDX-License-Identifier: AGPL-3.0-only
//! The Graphics page: what the compositor recommends for this computer and
//! what each profile changes.
//!
//! The chooser itself is the schema's `decoration.profile` enum, drawn by
//! `schema_page` like any other and applied live. This module supplies what
//! the schema cannot say: the compositor's recommendation and its reason
//! (read from `get_metrics`, never computed here), and a line for each
//! profile. Nothing is written by this page; the key is.

use iced::widget::{column, text, Column};
use iced::{Element, Theme};
use serde_json::Value;

use ec_abyss_config::gfx::GfxProfile;
use ec_ui::theme;
use ec_ui::tokens::{font, size, space};
use ec_ui::widget::{glass_panel as panel, hairline, list_row, micro_label};

use crate::app::{App, Message};

pub const KEY: &str = "decoration.profile";

/// The `graphics` object of `get_metrics`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    /// `decoration.profile` as written.
    pub setting: String,
    /// The profile in force.
    pub effective: String,
    pub recommended: Option<String>,
    pub reason: Option<String>,
    /// The governor lowered the profile for this session: (from, to).
    pub stepped_down: Option<(String, String)>,
}

impl Status {
    pub fn parse(metrics: &Value) -> Option<Status> {
        let g = metrics.get("graphics")?;
        let s = |k: &str| g.get(k).and_then(Value::as_str).map(str::to_owned);
        Some(Status {
            setting: s("setting")?,
            effective: s("effective")?,
            recommended: s("recommended"),
            reason: s("reason"),
            stepped_down: g.get("stepped_down").and_then(|d| {
                Some((
                    d.get("from")?.as_str()?.to_owned(),
                    d.get("to")?.as_str()?.to_owned(),
                ))
            }),
        })
    }

    /// `Recommended for this PC: low (older Intel integrated graphics)`.
    pub fn recommendation(&self) -> String {
        match (&self.recommended, &self.reason) {
            (Some(p), Some(r)) => format!("{p} ({r})"),
            (Some(p), None) => p.clone(),
            _ => "not measured yet".into(),
        }
    }
}

/// What a choice does, in a line. `auto` follows the recommendation.
pub fn blurb(choice: &str) -> Option<&'static str> {
    if choice == "auto" {
        return Some("Follow the recommendation for this PC. A setting you change yourself always wins.");
    }
    GfxProfile::parse(choice).map(GfxProfile::describe)
}

fn tertiary<'a>(t: String) -> Element<'a, Message, Theme> {
    text(t)
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_tertiary)
        .into()
}

fn primary<'a>(t: String) -> Element<'a, Message, Theme> {
    text(t).font(font::UI).size(size::BODY).into()
}

/// The recommendation card, then one line per choice.
pub fn hero(app: &App) -> Element<'_, Message, Theme> {
    let st = app.graphics.clone().unwrap_or_default();
    let mut body = Column::new()
        .push(list_row("Recommended for this PC", primary(st.recommendation())))
        .push(hairline())
        .push(list_row("In use now", primary(st.effective.clone())));
    if let Some((from, to)) = &st.stepped_down {
        body = body.push(hairline()).push(list_row(
            "Lowered this session",
            tertiary(format!("{from} to {to}, after missed frames")),
        ));
    }
    body = body.push(hairline()).push(
        iced::widget::container(micro_label("what each choice changes")).padding([space::ROW_Y, space::CARD]),
    );
    for choice in ["auto", "minimal", "low", "balanced", "full"] {
        body = body.push(hairline()).push(list_row(
            choice,
            tertiary(blurb(choice).unwrap_or_default().to_owned()),
        ));
    }
    column![panel(app.glass_radius, app.hairline_cards, body)].into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_choice_has_a_line() {
        for n in ec_abyss_config::gfx::ProfileSetting::NAMES {
            assert!(blurb(n).is_some_and(|b| !b.is_empty()), "{n}");
        }
        assert!(blurb("ultra").is_none());
    }

    #[test]
    fn status_reads_the_metrics_object() {
        let m = json!({"graphics": {"setting": "auto", "effective": "low", "recommended": "low",
            "reason": "older Intel integrated graphics",
            "stepped_down": {"from": "balanced", "to": "low"}}});
        let s = Status::parse(&m).unwrap();
        assert_eq!(s.recommendation(), "low (older Intel integrated graphics)");
        assert_eq!(s.stepped_down, Some(("balanced".into(), "low".into())));
        assert!(Status::parse(&json!({})).is_none());
        let bare = Status::parse(&json!({"graphics": {"setting": "full", "effective": "full"}})).unwrap();
        assert_eq!(bare.recommendation(), "not measured yet");
    }
}
