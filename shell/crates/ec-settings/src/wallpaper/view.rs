// SPDX-License-Identifier: AGPL-3.0-only
//! The Wallpaper page's blocks (`wallpaper.rs` says what they change).
//!
//! Hero: the displays as a canvas at their real place and size, each tile
//! saying whether it has its own picture; "All displays" sits above it, and a
//! display that is off or unplugged is a chip beneath. Beneath the hero is
//! one card for the display picked, or, for "All displays", the page's own
//! schema rows (the global picture, sizing and colour).
//!
//! Accent ledger: the one live yellow is the picked display's "Own picture"
//! state, which is the page's answer to "does this display differ?". The
//! choices are white (`pick_pill`), and the canvas tiles are clearer glass.

use std::path::Path;

use iced::widget::{column, container, row, text, Column, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use ec_abyss_config::schema::WALLPAPER_MODES;
use ec_ui::theme;
use ec_ui::tokens::{color, font, size, space};
use ec_ui::widget::arrangement_geometry::Rect;
use ec_ui::widget::{
    arrangement, arrangement_tile, color_picker, elide, fold, glass_inset, glass_panel as panel, hairline,
    micro_label, pick_pill, pill_group, row_caption, swatch_button, value as mono, Tile,
};

use super::{effective_path, output_of, scale_warning, Msg, Pane, COLOR, DEFAULT_MODE, MODE};
use crate::app::{App, Message};
use crate::output::Output;

/// Connector names read in full up to this many characters, then elided.
const NAME_CHARS: usize = 16;
/// The longest a file name shows before it is cut.
const FILE_CHARS: usize = 30;

/// `2 displays` and `1 with its own picture`, for the header chip.
pub fn status(app: &App) -> (String, String) {
    // Connected displays only: an unplugged override is counted in the measure.
    let shown = app.wall.listed.iter().filter(|l| l.connected).count();
    let state = match shown {
        0 => "no displays".to_owned(),
        1 => "1 display".to_owned(),
        n => format!("{n} displays"),
    };
    let own = app.wall.entries.len();
    let measure = match own {
        0 => "one picture for all".to_owned(),
        1 => "1 with its own picture".to_owned(),
        n => format!("{n} with their own pictures"),
    };
    (state, measure)
}

fn msg(m: Msg) -> Message {
    Message::Wall(m)
}

fn caption<'a>(s: impl Into<String>) -> Element<'a, Message, Theme> {
    text(s.into())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_tertiary)
        .into()
}

/// The hero: the canvas, its "All displays" choice, and the chips.
pub fn hero(app: &App) -> Element<'_, Message, Theme> {
    let pane = &app.wall;
    let sel = pane.selected.as_deref();

    // Only an output that is on has a place on the canvas; the rest are chips.
    let placed: Vec<(&Output, Rect)> = app
        .outputs
        .iter()
        .filter_map(|o| o.rect().map(|r| (o, r)))
        .collect();
    let names: Vec<String> = placed.iter().map(|(o, _)| o.name.clone()).collect();
    let tiles: Vec<Tile<'_, Message>> = placed
        .iter()
        .enumerate()
        .map(|(i, (o, rect))| Tile {
            id: i as u64,
            rect: *rect,
            content: arrangement_tile(
                i + 1,
                &elide(&o.name, NAME_CHARS),
                &reading(app, &o.name),
                sel == Some(o.name.as_str()),
                true,
            ),
        })
        .collect();

    let all = pick_pill("All displays", sel.is_none(), Some(msg(Msg::Select(None))));
    // The header chip already counts the overrides; the head only picks.
    let head = row![micro_label("displays"), Space::new().width(Length::Fill), all,]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);

    let mut body = Column::new().spacing(space::ROW_Y).push(head);
    if tiles.is_empty() {
        body = body.push(caption(
            "No display is on. Pick one below to set its picture for when it is.",
        ));
    } else {
        body = body.push(glass_inset(
            arrangement(tiles).on_select(move |i| msg(Msg::Select(names.get(i as usize).cloned()))),
        ));
    }

    // Off and unplugged displays have no place on the canvas, but they keep
    // their pictures and can still be picked.
    // White pills, not the chip's gold: the one live yellow is "Own picture".
    let off: Vec<Element<'_, Message, Theme>> = pane
        .listed
        .iter()
        .filter(|l| !placed.iter().any(|(o, _)| o.name == l.name))
        .map(|l| {
            let why = if l.connected { "off" } else { "not connected" };
            let label = format!("{} \u{b7} {why}", elide(&l.name, NAME_CHARS));
            pick_pill(
                &label,
                sel == Some(l.name.as_str()),
                Some(msg(Msg::Select(Some(l.name.clone())))),
            )
        })
        .collect();
    if !off.is_empty() {
        body = body.push(pill_group(off));
    }

    panel(app.glass_radius, app.hairline_cards, body)
}

/// A tile's reading: whose picture it shows, and how it is sized.
fn reading(app: &App, name: &str) -> String {
    match app.wall.entry(name) {
        Some(e) => {
            let mode = e.mode.clone().unwrap_or_else(|| global_mode(app));
            format!("own \u{b7} {mode}")
        }
        None => "all displays".to_owned(),
    }
}

fn global_mode(app: &App) -> String {
    app.key(MODE)
        .and_then(|k| k.value.as_str())
        .unwrap_or(DEFAULT_MODE)
        .to_owned()
}

fn global_color(app: &App) -> Option<String> {
    app.key(COLOR)
        .and_then(|k| k.value.as_str().or_else(|| k.default.as_str()))
        .map(str::to_owned)
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |f| f.to_string_lossy().into_owned())
}

/// `[r, g, b, a]` as the colour it is.
fn iced_color(c: [u8; 4]) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], f32::from(c[3]) / 255.0)
}

/// The palette's ground, for a background the compositor reports as nothing.
fn ground() -> [u8; 4] {
    let c = color::base();
    let ch = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    [ch(c.r), ch(c.g), ch(c.b), 255]
}

/// `WxH`, and the orientation a rotated output reads as.
fn size_line(o: &Output) -> Option<String> {
    let (w, h) = o.logical_size()?;
    let turn = if h > w { "portrait" } else { "landscape" };
    Some(format!("{w}\u{d7}{h} \u{b7} {turn}"))
}

/// One label over its source, at the left of a settings row.
fn lead<'a>(label: &str, source: &str) -> Element<'a, Message, Theme> {
    column![
        text(label.to_owned())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_secondary),
        caption(source.to_owned()),
    ]
    .spacing(space::LINE_GAP)
    .into()
}

/// A settings row: a label and its source at the left, the control at the right.
fn row_of<'a>(
    lead: Element<'a, Message, Theme>,
    value: Element<'a, Message, Theme>,
) -> Element<'a, Message, Theme> {
    container(fold(lead, value))
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .into()
}

/// The card for the picked display: its picture, sizing and background, each
/// showing what it inherits when it does not set its own.
pub fn card<'a>(app: &'a App, name: &'a str) -> Element<'a, Message, Theme> {
    let pane: &Pane = &app.wall;
    let out = output_of(app, name);
    let own = pane.entry(name);
    let gmode = global_mode(app);

    let state = match own {
        Some(_) => text("Own picture")
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL)
            .style(theme::text_accent)
            .into(),
        None => caption("Follows all displays"),
    };
    let mut head = row![
        micro_label(&elide(name, 24)),
        Space::new().width(Length::Fill),
        state,
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(Alignment::Center);
    if own.is_some() {
        head = head.push(pick_pill(
            "Use the same as all displays",
            false,
            Some(msg(Msg::Clear(name.to_owned()))),
        ));
    }

    let status_line = match out {
        Some(o) => {
            let off = if o.enabled { "" } else { " \u{b7} off" };
            format!("{}{off}", size_line(o).unwrap_or_else(|| "not on".to_owned()))
        }
        None => "Not connected. Its picture waits for the display.".to_owned(),
    };

    // The picture the display shows, and the warning when it is small.
    let picture = effective_path(app, name);
    let picture_source = if own.and_then(|e| e.path.as_ref()).is_some() {
        "this display"
    } else {
        "from all displays"
    };
    let picture_name = picture
        .as_deref()
        .map_or_else(|| "colour only".to_owned(), file_name);
    let picture_row = row_of(
        lead("Picture", picture_source),
        row![
            mono(&elide(&picture_name, FILE_CHARS)),
            pick_pill("Browse\u{2026}", false, Some(msg(Msg::Browse(name.to_owned())))),
        ]
        .spacing(space::PILL_GAP)
        .align_y(Alignment::Center)
        .into(),
    );
    let mode = own.and_then(|e| e.mode.clone()).unwrap_or_else(|| gmode.clone());
    let sizing_source = if own.and_then(|e| e.mode.as_ref()).is_some() {
        "this display"
    } else {
        "from all displays"
    };
    let sizing = pill_group(
        WALLPAPER_MODES
            .iter()
            .map(|m| {
                pick_pill(
                    crate::schema::value_label(m),
                    mode == *m,
                    Some(msg(Msg::Mode(name.to_owned(), (*m).to_owned()))),
                )
            })
            .collect(),
    );
    let sizing_row = row_of(lead("Sizing", sizing_source), sizing);

    // The background: the colour held in the picker while it is dragged,
    // else this display's, else the global one.
    let shown = pane
        .drag
        .as_ref()
        .filter(|(n, _)| n == name)
        .map(|(_, c)| *c)
        .or_else(|| own.and_then(|e| e.color.as_deref()).and_then(crate::schema::rgba))
        .or_else(|| global_color(app).as_deref().and_then(crate::schema::rgba))
        .unwrap_or_else(ground);
    let colour_source = if own.and_then(|e| e.color.as_ref()).is_some() {
        "this display"
    } else {
        "from all displays"
    };
    let open = pane.picker.as_deref() == Some(name);
    let colour_row = row_of(
        lead("Background", colour_source),
        row![
            swatch_button(iced_color(shown), open, msg(Msg::Picker(name.to_owned()))),
            mono(&crate::schema::hex(shown, false)),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center)
        .into(),
    );

    let mut col = Column::new()
        .spacing(space::ROW_Y)
        .push(head)
        .push(caption(status_line))
        .push(hairline())
        .push(picture_row);

    // A picture much smaller than the display it covers is called out, once,
    // under its row. Only "fill" covers by enlarging, so only fill is called out.
    if mode == "fill" {
        let note = picture
            .as_ref()
            .and_then(|p| pane.sizes.get(p).copied())
            .and_then(|pic| {
                let o = out?;
                let d = o.logical_size()?;
                scale_warning(pic, d).map(|s| (pic, s))
            });
        if let Some(((w, h), s)) = note {
            col = col.push(row_caption(&format!(
                "{w}\u{d7}{h} is enlarged {s:.1}\u{d7} to fill {name}, so it will look soft.",
            )));
        }
    }

    col = col
        .push(hairline())
        .push(sizing_row)
        .push(hairline())
        .push(colour_row);
    if open {
        // The picker outlives this borrow, so it takes its own copy of the name.
        let held = name.to_owned();
        col = col.push(
            container(color_picker(
                shown,
                false,
                move |c| msg(Msg::Drag(held.clone(), c)),
                msg(Msg::Release(name.to_owned())),
            ))
            .padding([space::ROW_Y, space::CARD]),
        );
    }
    if own.is_none() {
        col = col.push(row_caption(
            "Change a setting to give this display its own picture. Until then it shows the all-displays one.",
        ));
    }

    panel(app.glass_radius, app.hairline_cards, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_drops_the_directory() {
        assert_eq!(
            file_name("/usr/share/backgrounds/eclipseos/eclipse-dark.png"),
            "eclipse-dark.png"
        );
        assert_eq!(file_name("picture.png"), "picture.png");
    }

    #[test]
    fn the_fallback_ground_is_opaque() {
        assert_eq!(ground()[3], 255);
    }
}
