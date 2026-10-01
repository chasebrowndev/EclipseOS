// SPDX-License-Identifier: AGPL-3.0-only
//! The launcher pane: a prompt cell, a run of result cells, a hint line —
//! three blocks on one sheet of glass, none of them framed.
//!
//! The sheet is the compositor's material under a light tint
//! (`parts::surface`), and everything on it is cut from the one glass-cell
//! material the taskbar's chips are (`theme::glass_cell`): a faint white
//! fill, a hairline just inside the edge and a soft one-pixel drop. No
//! stroke on this pane is structure — no bordered band, no rules between
//! rows, no frame around the list. What separates things is air, and depth
//! is a cell lifting off the glass.
//!
//! Composition (`docs/COMPOSITION.md`): the hero is the **prompt cell** — the
//! live query at hero type in the tallest lozenge on the sheet, a mono marker
//! at its left and the cursor's place in the match list at its right, which
//! doubles as the machine's reading of the index (`1 / 37`). There is no
//! header strip above it: a launcher is a prompt, and a title plus a
//! two-line status chip stacked over the prompt read as a debug panel. Under
//! it the results are a **run of latent cells**: at rest a row is only its
//! text on the glass, and it becomes a cell only when it means something —
//! under the pointer it lifts into the white lozenge without taking the
//! selection, and the selected row rests as the focused one. So the three
//! blocks read as lozenge / floating run / line, and the hero is the only
//! cell that is always there. The hint line's keys sit in small glass caps
//! (`parts::key_hint`), never accented.
//!
//! **The accent ledger: the one live yellow is the selected row's name.**
//! Its cell is the focused glass — a clearer white fill and rim, no gold
//! ground and no glow — and its identifier sits beside it in tertiary mono.
//! That row is what Enter runs, which is the only state this pane has — the
//! drawn window scrolls with `selected` (see `scroll`) precisely so that the
//! statement stays true past the last drawn row. A selected row that cannot
//! launch (a terminal-only entry) wears the plain cell and a grey name
//! instead, and a refusal is `DANGER`, which is its own channel and not the
//! accent.
//!
//! Every colour and size comes from `ec_ui`; a literal anywhere in here
//! is a bug, with the exception of the layout metrics named at the top of the
//! file, which the surface height is derived from and which therefore cannot
//! live in a styling token.

use iced::widget::text::Wrapping;
use iced::widget::{button, container, row, text, text_input, Column, Row, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use ec_services::apps::Entry;
use ec_ui::theme::{self, CellTone};
use ec_ui::tokens::{color, font, radius, size, space};
use ec_ui::widget as parts;

use crate::app::{App, Message, INPUT_ID};
use crate::{MAX_ROWS, WIDTH};

/// Height of the hero prompt cell, its padding included.
const BAND_H: f32 = 52.0;
/// Height of one result cell.
const ROW_H: f32 = 36.0;
/// The air between two result cells. Air, not a rule: two lit cells side by
/// side (the selection and the row under the pointer) need a sliver of glass
/// between them or their drops merge into one slab.
const ROW_GAP: f32 = 2.0;
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
    (space::CARD * 2.0
        + BAND_H
        + space::BLOCK
        + ROW_H * MAX_ROWS as f32
        + ROW_GAP * (MAX_ROWS - 1) as f32
        + space::BLOCK
        + PROBLEM_H)
        .ceil() as u32
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    let body = Column::new()
        .spacing(space::BLOCK)
        .push(prompt(app))
        .push(results(app))
        .push(footer(app));

    // The sheet is the whole surface: the compositor masks it to
    // `decoration.rounding` and draws its material underneath, so there is
    // no outer padding for a blurred rim to show through.
    container(parts::surface(app.glass_radius, app.blur, body))
        .width(Length::Fixed(WIDTH as f32))
        .into()
}

/// The hero. The query at hero scale, with the cursor's place in the match
/// list at the right so that arrowing has a readable effect even when the
/// selected row is the one already on screen.
fn prompt(app: &App) -> Element<'_, Message, Theme> {
    let field = text_input("search applications", &app.query)
        .id(INPUT_ID)
        .on_input(Message::Query)
        // iced's `text_input` captures Enter itself, so the launcher's Enter
        // is wired here rather than in the key subscription.
        .on_submit(Message::Activate)
        .size(size::PROMPT)
        .font(font::UI)
        // The cell is the box and does the padding; a field that padded
        // itself as well would sit off-centre inside it.
        .padding(iced::Padding::ZERO)
        .style(theme::prompt_input);

    let position = if app.matched.is_empty() {
        "no match".to_owned()
    } else {
        format!("{} / {}", app.selected + 1, app.matched.len())
    };
    let position = text(position)
        .size(size::MONO)
        .font(font::DATA)
        .color(color::TEXT_TERTIARY);

    parts::prompt_cell("/", field, Some(position.into()))
        .height(Length::Fixed(BAND_H))
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
///
/// No container: the rows float on the sheet itself. A frame around them
/// would be a stroke doing the job the air between the blocks already does,
/// and a card holding cells is the card-in-card the spec rules out.
fn results(app: &App) -> Element<'_, Message, Theme> {
    let offset = scroll(app);
    let mut col = Column::new().spacing(ROW_GAP);
    for slot in 0..MAX_ROWS {
        let index = offset + slot;
        col = match app.matched.get(index) {
            Some(&entry) => col.push(entry_row(&app.entries[entry], index, index == app.selected)),
            // An empty slot rather than a shorter list: the panel is a fixed
            // height, and rows that slide up as you type are rows you misclick.
            None => col.push(Space::new().height(Length::Fixed(ROW_H)).width(Length::Fill)),
        };
    }
    col.into()
}

/// One row. `index` is an index into `matched`, not a slot on screen: the
/// drawn window scrolls, so a click has to name the match it lands on.
fn entry_row(entry: &Entry, index: usize, selected: bool) -> Element<'static, Message, Theme> {
    // A terminal-only entry is listed and greyed: `apps::launch` will refuse
    // it, and a row that is simply missing teaches the human nothing. The
    // greying wins over the selection: a selected row that cannot launch must
    // not read as a selected row that can.
    let live = selected && !entry.terminal;
    let name_tint = if entry.terminal {
        color::TEXT_TERTIARY
    } else if live {
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
    // Tertiary, not gold: the name already carries the row's one yellow.
    let tail: Element<'static, Message, Theme> = if live {
        text(entry.id.trim_end_matches(".desktop").to_owned())
            .size(size::MONO)
            .font(font::DATA)
            .wrapping(Wrapping::None)
            .color(color::TEXT_TERTIARY)
            .into()
    } else {
        Space::new().into()
    };

    let line = row![
        container(name).width(Length::Fixed(NAME_W)).clip(true),
        // The gap to the identifier has to be real padding: two strings that
        // meet at a clip edge read as one string. The clip has to sit on the
        // inner box, inset by that padding — a clip on the outer, padded
        // container clips at its full width, which is the same edge the
        // identifier starts at, so an unwrapped text node overflows straight
        // through the padding before the clip ever stops it.
        container(container(note).width(Length::Fill).clip(true))
            .width(Length::Fill)
            .padding(iced::Padding::ZERO.right(space::CARD)),
        tail,
    ]
    .align_y(Alignment::Center);

    // The focused glass for the row Enter runs, the plain lozenge for a
    // selected row that cannot run, and nothing at all for the rest until
    // the pointer lifts them — the taskbar chip's glass cell, at the sheet's
    // inset radius.
    let tone = if live {
        CellTone::Focused
    } else if selected {
        CellTone::Plain
    } else {
        CellTone::Latent
    };

    // The terminal row stays pressable on purpose: clicking it must put the
    // refusal on the footer rather than being silently inert. The press names
    // its row — the pointer may rest on a row the arrow keys have since moved
    // off, and a click runs what it lands on, not what the keyboard chose.
    let cell = button(container(line).height(Length::Fill).align_y(Alignment::Center))
        .width(Length::Fill)
        .height(Length::Fixed(ROW_H))
        .padding([0.0, space::CARD])
        .style(theme::glass_cell(tone, radius::INSET))
        .on_press(Message::Launch(index));

    cell.into()
}

/// The refusal or the key hints at the left, how much of the list is off the
/// bottom at the right. Plain text on the glass; only the keys are capped.
fn footer(app: &App) -> Element<'_, Message, Theme> {
    let left: Element<'_, Message, Theme> = match app.problem.as_ref() {
        // Verbatim, and in the warning colour: the human asked for something
        // and did not get it.
        Some(problem) => text(problem.clone())
            .size(size::BODY_SMALL)
            .font(font::DATA)
            .color(color::DANGER)
            .into(),
        None => Row::new()
            .spacing(space::HINT_GAP)
            .align_y(Alignment::Center)
            .push(parts::key_hint("\u{2191}\u{2193}", "select"))
            .push(parts::key_hint("enter", "run"))
            .push(parts::key_hint("esc", "close"))
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
