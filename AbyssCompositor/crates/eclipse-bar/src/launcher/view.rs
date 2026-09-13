// SPDX-License-Identifier: AGPL-3.0-only
//! The launcher pane: a header strip, a prompt band, a result list, a footer.
//!
//! Composition (`docs/COMPOSITION.md`): the hero is a **prompt band** — a
//! tall bordered block holding the live query at hero type size, a mono
//! marker at its left and the cursor's position in the match list at its
//! right. It is the block this pane is about, and it deliberately shares no
//! silhouette with the hairline list beneath it. The strip above and the line
//! below are borderless, so the four blocks read as strip / band / list /
//! line rather than as a stack of panels.
//!
//! **The accent ledger: the one live yellow is the selected result row** —
//! its 3px bar, its ground tint and its name. That row is what Enter runs,
//! which is the only state this pane has — the drawn window scrolls with
//! `selected` (see `scroll`) precisely so that the statement stays true past
//! the eighth match. The band's border is
//! `BORDER_STRONG`, the header chip is mono grey, and a refusal is `DANGER`,
//! which is its own channel and not the accent.
//!
//! Every colour and size comes from `eclipse_ui`; a literal anywhere in here
//! is a bug, with the exception of the layout metrics named at the top of the
//! file, which the surface height is derived from and which therefore cannot
//! live in a styling token.

use iced::widget::text::Wrapping;
use iced::widget::{container, mouse_area, row, text, text_input, Column, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use eclipse_services::apps::Entry;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget as parts;

use crate::launcher::app::{App, Message, INPUT_ID};
use crate::launcher::{MAX_ROWS, WIDTH};

/// Padding between the panel and the edge of its surface.
const OUTER: f32 = 10.0;
/// Height of the header strip: a micro label beside a two-line status chip.
const HEADER_H: f32 = 32.0;
/// Height of the hero band, its border and padding included.
const BAND_H: f32 = 52.0;
/// Height of one result row.
const ROW_H: f32 = 40.0;
/// The hairline drawn above every row but the first.
const HAIR: f32 = 1.0;
/// Width of the name column. A list whose second column starts at a
/// different x on every row is not a list, so this is a layout metric and not
/// a styling token.
const NAME_W: f32 = 196.0;
/// The last line of the panel, where a refusal, the key hints and the match
/// count go. It is reserved whether or not there is anything to say: a panel
/// that grows a line when a launch fails moves every row under the pointer
/// that just clicked.
const PROBLEM_H: f32 = 28.0;

/// The launcher's surface height, in whole pixels.
///
/// Constant for the life of the launcher. A layer surface fixes its size
/// before the boot fn runs, so a height that depended on how many entries
/// matched would need a `SizeChange` round trip on every keystroke.
pub fn surface_height() -> u32 {
    (OUTER * 2.0
        + space::CARD * 2.0
        + HEADER_H
        + space::BLOCK
        + BAND_H
        + space::BLOCK
        + ROW_H * MAX_ROWS as f32
        + HAIR * (MAX_ROWS - 1) as f32
        + space::BLOCK
        + PROBLEM_H)
        .ceil() as u32
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    let body = Column::new()
        .spacing(space::BLOCK)
        .push(header(app))
        .push(band(app))
        .push(results(app))
        .push(footer(app));

    container(parts::surface(body))
        .width(Length::Fixed(WIDTH as f32))
        .padding(OUTER)
        .into()
}

/// The header strip: what this surface is, and the machine's own reading of
/// the index it is searching.
fn header(app: &App) -> Element<'_, Message, Theme> {
    let state = if app.query.is_empty() {
        "all applications".to_owned()
    } else {
        format!("{} matching", app.matched.len())
    };
    let measure = format!("{} indexed", app.entries.len());

    container(
        row![
            parts::micro_label("launch"),
            Space::new().width(Length::Fill),
            parts::status_chip(&state, &measure),
        ]
        .align_y(Alignment::Center),
    )
    .height(Length::Fixed(HEADER_H))
    .align_y(Alignment::Center)
    .into()
}

/// The hero. The query at hero scale, with the cursor's place in the match
/// list at the right so that arrowing has a readable effect even when the
/// selected row is the one already on screen.
fn band(app: &App) -> Element<'_, Message, Theme> {
    let field = text_input("search applications", &app.query)
        .id(INPUT_ID)
        .on_input(Message::Query)
        // iced's `text_input` captures Enter itself, so the launcher's Enter
        // is wired here rather than in the key subscription.
        .on_submit(Message::Activate)
        .size(size::PROMPT)
        .font(font::UI)
        // The band is the box and does the padding; a field that padded
        // itself as well would sit off-centre inside it.
        .padding(iced::Padding::ZERO)
        .style(eclipse_ui::theme::prompt_input);

    let position = if app.matched.is_empty() {
        "no match".to_owned()
    } else {
        format!("{} / {}", app.selected + 1, app.matched.len())
    };
    let position = text(position)
        .size(size::MONO)
        .font(font::DATA)
        .color(color::TEXT_TERTIARY);

    container(parts::prompt_band("/", field, Some(position.into())))
        .height(Length::Fixed(BAND_H))
        .align_y(Alignment::Center)
        .into()
}

/// How far the drawn window has scrolled down the match list.
///
/// Derived, not stored: `matched` and `selected` stay the only state this
/// pane has, and a stored offset would be a third thing to keep consistent
/// with them across a refilter. The rule is the smallest one that keeps the
/// selection drawn — scroll only as far as it takes to bring `selected` back
/// inside the window — so the selection rides the bottom row going down and
/// the top row coming back up, and the accent is never off the pane.
fn scroll(app: &App) -> usize {
    app.selected.saturating_sub(MAX_ROWS - 1)
}

/// The visible slice of the match list, always `MAX_ROWS` tall.
fn results(app: &App) -> Element<'_, Message, Theme> {
    let offset = scroll(app);
    let mut col = Column::new();
    for slot in 0..MAX_ROWS {
        let index = offset + slot;
        // A hairline only between rows that exist. Ruling the empty tail as
        // well draws a ladder of blank rows, which reads as eight results of
        // which six failed to load.
        if slot > 0 && index < app.matched.len() {
            col = col.push(parts::hairline());
        } else if slot > 0 {
            col = col.push(container(Space::new()).height(Length::Fixed(HAIR)));
        }
        col = match app.matched.get(index) {
            Some(&entry) => col.push(entry_row(&app.entries[entry], index, index == app.selected)),
            // An empty slot rather than a shorter list: the panel is a fixed
            // height, and rows that slide up as you type are rows you misclick.
            None => col.push(
                container(Space::new())
                    .height(Length::Fixed(ROW_H))
                    .width(Length::Fill),
            ),
        };
    }
    parts::inset(col).into()
}

/// One row. `index` is an index into `matched`, not a slot on screen: the
/// drawn window scrolls, so a hover has to name the match it is over.
fn entry_row(entry: &Entry, index: usize, selected: bool) -> Element<'static, Message, Theme> {
    // A terminal-only entry is listed and greyed: `apps::launch` will refuse
    // it, and a row that is simply missing teaches the human nothing. The
    // greying wins over the selection: a selected row that cannot launch must
    // not read as a selected row that can.
    let name_tint = if entry.terminal {
        color::TEXT_TERTIARY
    } else if selected {
        color::ACCENT_TEXT
    } else {
        color::TEXT
    };
    let name = text(entry.name.clone())
        .size(size::BODY)
        .font(if selected { font::UI_MEDIUM } else { font::UI })
        .wrapping(Wrapping::None)
        .color(name_tint);

    // One line, clipped. A comment that wraps makes its row taller than its
    // neighbours, and a list whose rows are different heights is a list you
    // cannot arrow down by eye.
    let note = if entry.terminal {
        "needs a terminal".to_owned()
    } else {
        entry.comment.clone().unwrap_or_default()
    };
    let note = text(note)
        .size(size::BODY_SMALL)
        .font(font::UI)
        .wrapping(Wrapping::None)
        .color(if entry.terminal {
            color::TEXT_TERTIARY
        } else {
            color::TEXT_SECONDARY
        });

    // The identifier is on the selected row only. It is what will actually be
    // spawned, so it belongs beside the choice — and printing all eight makes
    // a column of mono noise that reads louder than the names it labels.
    let tail: Element<'static, Message, Theme> = if selected && !entry.terminal {
        text(entry.id.trim_end_matches(".desktop").to_owned())
            .size(size::MONO)
            .font(font::DATA)
            .wrapping(Wrapping::None)
            .color(color::ACCENT_TEXT)
            .into()
    } else {
        Space::new().into()
    };

    let line = row![
        container(name).width(Length::Fixed(NAME_W)).clip(true),
        // The note is clipped, so the gap to the identifier has to be real
        // padding: two strings that meet at a clip edge read as one string.
        container(note)
            .width(Length::Fill)
            .padding(iced::Padding::ZERO.right(space::CARD))
            .clip(true),
        tail,
    ]
    .align_y(Alignment::Center);

    let line = container(line)
        .height(Length::Fixed(ROW_H))
        .width(Length::Fill)
        // The bar occupies the first 3px of the row, so the text keeps the
        // same left edge whether or not this row is the selected one.
        .padding(iced::Padding {
            right: space::CARD,
            left: space::CARD - space::BAR_W,
            ..iced::Padding::ZERO
        })
        .align_y(Alignment::Center);

    // The terminal row keeps its mouse area on purpose: clicking it must put
    // the refusal on the footer rather than being silently inert.
    mouse_area(
        container(parts::choice_row(line, selected))
            .height(Length::Fixed(ROW_H))
            .width(Length::Fill),
    )
    .on_enter(Message::Select(index))
    .on_press(Message::Activate)
    .into()
}

/// The refusal or the key hints at the left, how much of the list is off the
/// bottom at the right.
fn footer(app: &App) -> Element<'_, Message, Theme> {
    let left: Element<'_, Message, Theme> = match app.problem.as_ref() {
        // Verbatim, and in the warning colour: the human asked for something
        // and did not get it.
        Some(problem) => text(problem.clone())
            .size(size::BODY_SMALL)
            .font(font::DATA)
            .color(color::DANGER)
            .into(),
        // Not `micro_label`: its per-character tracking is for one- or
        // two-word labels, and applied to a sentence it reads as a ransom
        // note.
        None => text("\u{2191}\u{2193} select \u{00b7} enter run \u{00b7} esc close")
            .size(size::MICRO)
            .font(font::DATA)
            .color(color::TEXT_TERTIARY)
            .into(),
    };

    // Both counts come from the scroll position, so the line changes as the
    // selection descends. Computed from `matched.len()` alone it would name a
    // constant, which is what made a scrolled-away selection invisible.
    let offset = scroll(app);
    let above = offset;
    let below = app.matched.len().saturating_sub(offset + MAX_ROWS);
    let tally = match (above, below) {
        (0, 0) => String::new(),
        (0, below) => format!("+{below} below"),
        (above, 0) => format!("+{above} above"),
        (above, below) => format!("+{above} above \u{00b7} +{below} below"),
    };
    let right: Element<'_, Message, Theme> = if tally.is_empty() {
        Space::new().into()
    } else {
        text(tally)
            .size(size::MONO)
            .font(font::DATA)
            .color(color::TEXT_TERTIARY)
            .into()
    };

    container(row![left, Space::new().width(Length::Fill), right].align_y(Alignment::Center))
        .height(Length::Fixed(PROBLEM_H))
        .padding([0.0, space::CARD])
        .align_y(Alignment::Center)
        .into()
}

/// The panel draws itself; the surface behind it is nothing at all, so it
/// floats over whatever is on screen rather than sitting on a sheet.
pub fn style(_app: &App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The surface has to be tall enough for every row it draws, or the last
    /// one is clipped off the bottom of the screen.
    #[test]
    fn the_surface_holds_every_row() {
        assert!(surface_height() as f32 >= BAND_H + ROW_H * MAX_ROWS as f32 + PROBLEM_H);
    }

    fn app_with(matched: usize, selected: usize) -> App {
        let mut app = App::new();
        app.matched = (0..matched).collect();
        app.selected = selected;
        app
    }

    /// The accent is what Enter runs, so the drawn window has to contain the
    /// selection for every reachable value of it.
    #[test]
    fn the_drawn_window_always_contains_the_selection() {
        for selected in 0..64 {
            let app = app_with(285, selected);
            let offset = scroll(&app);
            assert!(offset <= app.selected, "selection above the window");
            assert!(app.selected < offset + MAX_ROWS, "selection below the window");
        }
    }

    /// A list that fits does not scroll: the first match stays on the first
    /// row while the human arrows down it.
    #[test]
    fn a_list_that_fits_does_not_scroll() {
        assert_eq!(scroll(&app_with(MAX_ROWS, MAX_ROWS - 1)), 0);
    }
}
