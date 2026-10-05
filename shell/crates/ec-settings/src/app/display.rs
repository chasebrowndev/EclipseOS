// SPDX-License-Identifier: AGPL-3.0-only
//! The Display page: an arrangement canvas over a card for the selected
//! output (COMP-03 §1, COMP-13 §2.1).
//!
//! Hero: a spatial canvas of the outputs at their real relative size and
//! place, draggable. Beneath it, one card for the selected output. The accent
//! ledger: the one live yellow is the selected option in each choice (the
//! current mode, scale and rotation) — the page's "what is true now"; the
//! canvas itself stays white, the selected tile only a clearer glass.
//!
//! Nothing here is persisted by this app: every control is a `set_output`
//! call, and the compositor writes `outputs.kdl` itself (COMP-03 §4).

use iced::widget::{row, Column, Row, Space};
use iced::{Element, Length, Theme};
use serde_json::json;

use ec_ui::tokens::space;
use ec_ui::widget::{
    arrangement, arrangement_tile, big_value, chip, hairline, list_row, micro_label, panel, pill, pill_group,
    row_caption, value as mono, Tile, Toggle,
};

use super::{scale_control, App, Message, Num, INSET_MAX, INSET_SPAN};
use crate::output::{Edge, Output};

const TRANSFORMS: &[(&str, &str)] = &[
    ("normal", "Normal"),
    ("90", "90\u{b0}"),
    ("180", "180\u{b0}"),
    ("270", "270\u{b0}"),
];
const SCALE_PRESETS: &[f64] = &[1.0, 1.25, 1.5, 1.75, 2.0, 3.0];

impl App {
    /// The output the card is about: the one picked, else the focused one,
    /// else the first.
    pub(super) fn selected_output(&self) -> Option<&Output> {
        self.selected
            .and_then(|id| self.outputs.iter().find(|o| o.id == id))
            .or_else(|| self.outputs.iter().find(|o| o.focused))
            .or_else(|| self.outputs.first())
    }
}

/// `2 displays` / the selected output's reading, for the header chip.
pub(super) fn status(app: &App) -> (String, String) {
    let on = app.outputs.iter().filter(|o| o.enabled).count();
    let state = match on {
        1 => "1 display".to_owned(),
        n => format!("{n} displays"),
    };
    let measure = app
        .selected_output()
        .map(|o| format!("{} \u{b7} {}", o.name, o.mode_display()))
        .unwrap_or_else(|| "no outputs".into());
    (state, measure)
}

pub(super) fn blocks(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let Some(sel) = app.selected_output() else {
        return vec![panel(app.glass_radius, mono("no outputs")).into()];
    };
    vec![canvas(app, sel.id), card(app, sel)]
}

/// The hero: the arrangement, and beside it any output that is off (it has
/// no place in the layout, but it can still be picked and turned on).
fn canvas(app: &App, selected: u64) -> Element<'_, Message, Theme> {
    let placed: Vec<(usize, &Output)> = app
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.rect().is_some())
        .collect();

    let tiles: Vec<Tile<'_, Message>> = placed
        .iter()
        .map(|(i, o)| Tile {
            id: o.id,
            rect: o.rect().expect("filtered on a place"),
            content: arrangement_tile(
                i + 1,
                &o.name,
                &o.mode.map(|m| m.resolution()).unwrap_or_default(),
                o.id == selected,
                o.enabled,
            ),
        })
        .collect();

    let stage = ec_ui::widget::inset(
        arrangement(tiles)
            .on_select(Message::OutputSelected)
            .on_drop(Message::OutputMoved),
    );

    let mut footer = Row::new()
        .spacing(space::PILL_GAP)
        .align_y(iced::Alignment::Center);
    for (i, o) in app.outputs.iter().enumerate().filter(|(_, o)| o.rect().is_none()) {
        footer = footer.push(chip(
            Some(i + 1),
            &format!("{} \u{b7} off", o.name),
            o.id == selected,
            Message::OutputSelected(o.id),
        ));
    }
    let caption = if placed.len() > 1 {
        "drag to reposition"
    } else {
        "one output \u{b7} nothing to arrange"
    };

    panel(
        app.glass_radius,
        Column::new()
            .spacing(space::ROW_Y)
            .push(row![
                micro_label("arrangement"),
                Space::new().width(Length::Fill),
                mono(caption)
            ])
            .push(stage)
            .push(footer),
    )
    .into()
}

/// The selected output: toggle, mode, scale, rotation, identity, overscan.
fn card<'a>(app: &'a App, o: &'a Output) -> Element<'a, Message, Theme> {
    let id = o.id;
    let scale = app.scales.get(&id).copied().unwrap_or(o.scale);
    let inset = app.insets.get(&id).copied().unwrap_or(o.overscan);

    // Resolution: one pill each. Choosing one keeps the current refresh if
    // that resolution has it, else takes the fastest.
    let current = o.mode;
    let resolutions = pill_group(
        o.resolutions()
            .into_iter()
            .map(|(w, h)| {
                let wanted = o
                    .refreshes(w, h)
                    .into_iter()
                    .find(|m| Some(m.mhz) == current.map(|c| c.mhz))
                    .or_else(|| o.refreshes(w, h).into_iter().next());
                let label = crate::output::Mode { w, h, mhz: 0 }.resolution();
                let selected = current.is_some_and(|c| (c.w, c.h) == (w, h));
                match wanted {
                    Some(m) => pill(&label, selected, Message::OutputMode(id, m.wire())),
                    None => pill(&label, selected, Message::Reload),
                }
            })
            .collect(),
    );
    let refreshes = pill_group(
        current
            .map(|c| o.refreshes(c.w, c.h))
            .unwrap_or_default()
            .into_iter()
            .map(|m| {
                pill(
                    &m.refresh(),
                    Some(m.mhz) == current.map(|c| c.mhz),
                    Message::OutputMode(id, m.wire()),
                )
            })
            .collect(),
    );

    let presets = pill_group(
        SCALE_PRESETS
            .iter()
            .map(|p| {
                pill(
                    &format!("{}%", (p * 100.0).round()),
                    (scale - p).abs() < 0.005,
                    Message::OutputScaleSet(id, *p),
                )
            })
            .collect(),
    );
    let rotation = pill_group(
        TRANSFORMS
            .iter()
            .map(|(t, label)| {
                pill(
                    label,
                    o.transform == *t,
                    Message::OutputTransform(id, (*t).to_string()),
                )
            })
            .collect(),
    );

    let mut edges = Column::new().spacing(space::ROW_Y);
    for edge in Edge::ALL {
        let edge = *edge;
        let num = Num::Inset(id, edge);
        let draft = app.nums.get(&num);
        let current = inset.get(edge) as f64;
        let shown = draft.cloned().unwrap_or_else(|| INSET_SPAN.format(current));
        let invalid = draft.is_some_and(|d| INSET_SPAN.parse(d).is_none());
        let typed = num.clone();
        edges = edges.push(list_row(
            &crate::schema::sentence_case(edge.label()),
            ec_ui::widget::NumericSlider::new(
                0.0..=INSET_MAX,
                current,
                shown,
                move |v| Message::InsetMoved(id, edge, v),
                move |t| Message::NumberTyped(typed.clone(), t),
            )
            .step(1.0)
            .on_release(Message::InsetReleased(id))
            .on_commit(Message::NumberCommitted(num))
            .invalid(invalid),
        ));
    }

    let calibrating = app.calibrating == Some(id);
    let actions: Row<'_, Message, Theme> = if calibrating {
        row![
            pill("Commit", true, Message::Calibrate(id, "commit")),
            pill("Cancel", false, Message::Calibrate(id, "cancel")),
        ]
    } else {
        row![pill("Calibrate", false, Message::Calibrate(id, "start"))]
    }
    .spacing(space::PILL_GAP);

    let mut col = Column::new()
        .spacing(space::ROW_Y)
        .push(
            row![
                micro_label(&o.name),
                Space::new().width(Length::Fill),
                big_value(
                    &o.mode
                        .map(|m| m.refresh().replace(" Hz", ""))
                        .unwrap_or_else(|| "\u{2014}".into()),
                    "Hz",
                    false,
                ),
            ]
            .align_y(iced::Alignment::Center),
        )
        .push(list_row(
            "Enabled",
            Toggle::new(o.enabled, move |on| Message::OutputEnabled(id, on)),
        ))
        .push(list_row("Resolution", resolutions))
        .push(list_row("Refresh", refreshes));
    if o.modes.is_empty() {
        col = col.push(row_caption(
            "The compositor reports only the current mode, so no others are offered yet.",
        ));
    }
    col = col
        .push(list_row("Scale", presets))
        .push(list_row("Custom scale", scale_control(app, id, scale)))
        .push(list_row("Rotation", rotation))
        .push(hairline())
        .push(list_row("Connector", mono(&o.name)))
        .push(list_row(
            "Identity",
            mono(if o.identity.is_empty() {
                "\u{2014}"
            } else {
                &o.identity
            }),
        ))
        .push(list_row("Position", mono(&o.position_display())))
        .push(hairline())
        .push(micro_label("overscan"))
        .push(edges)
        .push(actions);

    panel(app.glass_radius, col).into()
}

impl App {
    /// A drop on the canvas: one `set_output {position}`, on release.
    pub(super) fn move_output(&mut self, id: u64, x: i64, y: i64) {
        self.set_output(id, "position", json!({"x": x, "y": y}));
    }

    pub(super) fn pick_output(&mut self, id: u64) {
        if self.outputs.iter().any(|o| o.id == id) {
            self.selected = Some(id);
        }
    }
}
