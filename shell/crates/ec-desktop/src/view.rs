// SPDX-License-Identifier: AGPL-3.0-only
//! What a desktop surface draws: the icons of `~/Desktop` in their grid, the
//! selection over them, and the small things that float above — a rubber
//! band, a drag's ghosts, the context menu, the launch prompt, the failure
//! note. All of it is the model's (`app::App::desktop` and `app::App::ui`);
//! the view never touches the disk.
//!
//! Shape: a ground that hears the pointer, with every icon pinned to its
//! cell over it, then the floating sheets. Nothing here is a pane, so there
//! is no header and no hero; the hero is the grid itself.
//!
//! Accent ledger: none. A desktop shows no live value of its own; selection
//! is white glass (the focused cell's fill and a crisp rim), a failure is
//! the danger colour on a bar, and nothing is gold.

use std::path::PathBuf;

use ec_ui::theme::{self, CellTone};
use ec_ui::tokens::{color, desktop, font, menu, radius, size, space};
use ec_ui::widget as parts;
use ec_ui::widget::CellState;
use iced::widget::{button, column, container, mouse_area, pin, row, text, text_input, Space};
use iced::window::Id;
use iced::{Alignment, Color, Element, Length, Point, Theme};

use crate::app::{App, Message, RENAME_ID};
use crate::scan::{Entry, Kind};
use crate::ui::{Pointer, Row, Scene, Verb};

type El<'a> = Element<'a, Message, Theme>;

pub fn view(app: &App, id: Id) -> El<'_> {
    let ground = || container(Space::new()).width(Length::Fill).height(Length::Fill);
    let Some(size) = app.surfaces.get(&id).and_then(|s| s.size) else {
        return ground().into();
    };
    let output = app.surfaces.get(&id).map_or("", |s| s.output.as_str());
    let scene = Scene::new(&app.desktop, output, size);
    // Menus, drags and the rename field live on the surface the pointer is on.
    let here = app.active == Some(id);
    let ui = &app.ui;

    let mut layers: Vec<El<'_>> = vec![mouse_area(ground())
        .on_move(move |p| Message::Pointer(id, Pointer::Moved(p)))
        .on_press(Message::Pointer(id, Pointer::Pressed))
        .on_release(Message::Pointer(id, Pointer::Released))
        .on_right_press(Message::Pointer(id, Pointer::RightPressed))
        .on_exit(Message::Pointer(id, Pointer::Exited))
        .into()];

    let dragging = here.then_some(ui.drag.as_ref()).flatten();
    let drop_onto = dragging.and_then(|d| {
        scene
            .at(d.at)
            .filter(|i| i.entry.kind == Kind::Dir && !d.names.contains(&i.entry.name))
            .map(|i| i.entry.name.as_str())
    });

    for item in &scene.items {
        let name = item.entry.name.as_str();
        let selected = ui.selected.contains(name);
        let lit =
            drop_onto == Some(name) || (here && dragging.is_none() && ui.hover.as_deref() == Some(name));
        let state = match (selected, lit) {
            (true, true) => CellState::SelectedHover,
            (true, false) => CellState::Selected,
            (false, true) => CellState::Hover,
            (false, false) => CellState::Rest,
        };
        let left_behind = dragging.is_some_and(|d| d.names.iter().any(|n| n == name));
        let alpha = if left_behind {
            desktop::DRAG_REST_ALPHA
        } else {
            1.0
        };
        let renaming = here
            .then_some(ui.rename.as_ref())
            .flatten()
            .filter(|r| r.from == name);
        layers.push(
            pin(tile(app, item.entry, state, alpha, renaming))
                .x(item.rect.x)
                .y(item.rect.y)
                .into(),
        );
    }

    if let Some(d) = dragging {
        let by = d.at - d.grab;
        for n in &d.names {
            if let Some(item) = scene.find(n) {
                layers.push(
                    pin(tile(
                        app,
                        item.entry,
                        CellState::Selected,
                        desktop::DRAG_GHOST_ALPHA,
                        None,
                    ))
                    .x(item.rect.x + by.x)
                    .y(item.rect.y + by.y)
                    .into(),
                );
            }
        }
    }

    if let Some(band) = here.then_some(ui.band.as_ref()).flatten() {
        let r = band.rect();
        layers.push(pin(parts::rubber_band(r.width, r.height)).x(r.x).y(r.y).into());
    }

    if let Some(item) = here
        .then_some(ui.prompt.as_deref())
        .flatten()
        .and_then(|n| scene.find(n))
    {
        let at = prompt_origin(item.rect, size);
        layers.push(pin(prompt(item.entry)).x(at.x).y(at.y).into());
    }

    if let Some(toast) = ui.toast.as_ref().filter(|_| app.active.is_none_or(|a| a == id)) {
        layers.push(
            pin(note(&toast.text))
                .x((size.width - desktop::TOAST_W - desktop::MARGIN).max(0.0))
                .y((size.height - desktop::TOAST_H - desktop::MARGIN).max(0.0))
                .into(),
        );
    }

    if let Some(m) = here.then_some(ui.menu.as_ref()).flatten() {
        let rows = ui.menu_rows(&scene);
        let at = m.origin(size, &rows);
        layers.push(pin(context_menu(&rows, scene.sort)).x(at.x).y(at.y).into());
    }

    iced::widget::stack(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The runtime's own clear under every surface: transparent, so the wallpaper
/// below shows through.
pub fn style(_app: &App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}

/// The letters a missing icon carries: its extension, or what kind of thing it
/// is. Never the file's name.
pub fn badge(entry: &Entry) -> String {
    match entry.kind {
        Kind::Dir => "DIR".to_owned(),
        Kind::Launcher => "APP".to_owned(),
        Kind::Symlink if entry.broken => "LINK".to_owned(),
        Kind::Symlink if entry.to_dir => "DIR".to_owned(),
        Kind::Symlink | Kind::File => entry
            .name
            .rsplit_once('.')
            .filter(|(stem, ext)| {
                !stem.is_empty() && !ext.is_empty() && ext.chars().count() <= desktop::BADGE_CHARS
            })
            .map_or_else(|| "FILE".to_owned(), |(_, ext)| ext.to_uppercase()),
    }
}

/// One icon in its cell: art over label, or art over the rename field.
fn tile<'a>(
    app: &'a App,
    entry: &'a Entry,
    state: CellState,
    alpha: f32,
    renaming: Option<&'a crate::ui::Rename>,
) -> El<'a> {
    let file: Option<PathBuf> = app.desktop.icons.get(&entry.name).cloned().flatten();
    let broken = entry.kind == Kind::Symlink && entry.broken;
    let art = parts::icon_art(file, &badge(entry), alpha, broken);
    let label: El<'a> = match renaming {
        Some(r) => text_input("", &r.text)
            .id(RENAME_ID)
            .on_input(Message::RenameInput)
            .on_submit(Message::RenameSubmit)
            .size(size::BODY_SMALL)
            .font(font::UI_MEDIUM)
            .padding([space::BADGE_Y, space::BADGE_X])
            .style(if r.valid() {
                theme::eclipse_input
            } else {
                theme::eclipse_input_invalid
            })
            .into(),
        None => parts::icon_label(&crate::grid::label(entry.label()), alpha, broken),
    };
    parts::icon_cell(art, label, state)
}

/// Where the launch prompt hangs: centred under its icon, pulled inside the
/// surface, and above the icon instead when there is no room below.
pub fn prompt_origin(cell: iced::Rectangle, size: iced::Size) -> Point {
    let x = (cell.x + (cell.width - desktop::PROMPT_W) / 2.0)
        .min(size.width - desktop::PROMPT_W - desktop::MARGIN)
        .max(desktop::MARGIN.min(size.width));
    let below = cell.y + cell.height + desktop::PROMPT_GAP;
    let y = if below + desktop::PROMPT_H + desktop::MARGIN <= size.height {
        below
    } else {
        (cell.y - desktop::PROMPT_H - desktop::PROMPT_GAP).max(0.0)
    };
    Point::new(x, y)
}

/// "Allow launching?": the inline question for a launcher whose exec bit is
/// not set. Only the second button allows it.
fn prompt(entry: &Entry) -> El<'_> {
    let name = crate::grid::label(entry.label()).replace('\n', " ");
    let ask = |label: &str, msg: Message| -> El<'_> {
        button(
            container(
                text(label.to_owned())
                    .size(size::BODY_SMALL)
                    .font(font::UI_MEDIUM),
            )
            .width(Length::Fill)
            .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([space::PILL_Y, space::PILL_X])
        .style(theme::glass_cell(CellTone::Plain, radius::INSET))
        .on_press(msg)
        .into()
    };
    let body = column![
        text("Allow launching?")
            .size(size::BODY)
            .font(font::UI_SEMIBOLD)
            .color(color::text()),
        text(format!("{name} has not been allowed to run."))
            .size(size::BODY_SMALL)
            .font(font::UI)
            .color(color::text_secondary()),
        row![ask("Not now", Message::KeepLocked), ask("Allow", Message::Allow)].spacing(space::PILL_GAP),
    ]
    .spacing(space::TITLE_GAP);
    swallow(parts::desk_sheet(desktop::PROMPT_W, space::ROW_Y, body))
}

/// A failure, once: a glass note in the corner, a red bar at its edge.
fn note(words: &str) -> El<'_> {
    let body = row![
        parts::edge_quad(Length::Fixed(space::BAR_W), Length::Fill, color::danger()),
        text(words.to_owned())
            .size(size::BODY_SMALL)
            .font(font::UI)
            .color(color::text()),
    ]
    .spacing(space::HEADER_GAP)
    .height(Length::Fixed(desktop::TOAST_H - 2.0 * space::ROW_Y));
    mouse_area(parts::desk_sheet(desktop::TOAST_W, space::ROW_Y, body))
        .on_press(Message::DismissToast)
        .into()
}

/// Keep a click on a sheet from falling through to the desktop below it.
fn swallow(sheet: El<'_>) -> El<'_> {
    mouse_area(sheet)
        .on_press(Message::Swallow)
        .on_right_press(Message::Swallow)
        .into()
}

fn context_menu(rows: &[Row], sort: crate::layout::Sort) -> El<'static> {
    let mut list = column![];
    for r in rows {
        list = list.push(match r {
            Row::Verb(v) => {
                let mark: El<'static> = match v {
                    Verb::Sort(s) => parts::desk_menu_tick(*s == sort),
                    other => verb_mark(*other),
                };
                parts::desk_menu_item(v.label(), mark, *v == Verb::Trash, Message::Chose(*v))
            }
            Row::Rule => parts::menu_separator(),
            Row::Heading(h) => parts::desk_menu_heading(h),
        });
    }
    swallow(parts::desk_sheet(menu::W, menu::PAD, list))
}

/// The mark that leads a verb: its symbolic icon, tertiary ink; red on the
/// one that trashes.
fn verb_mark(verb: Verb) -> El<'static> {
    let (name, tint) = match verb {
        Verb::Open => ("document-open", color::text_tertiary()),
        Verb::Rename => ("document-edit", color::text_tertiary()),
        Verb::Copy => ("edit-copy", color::text_tertiary()),
        Verb::Trash => ("user-trash", color::danger()),
        Verb::AllowLaunching => ("emblem-ok", color::text_tertiary()),
        Verb::NewFolder => ("folder-new", color::text_tertiary()),
        Verb::NewFile => ("document-new", color::text_tertiary()),
        Verb::Terminal => ("utilities-terminal", color::text_tertiary()),
        Verb::Settings => ("preferences-system", color::text_tertiary()),
        Verb::Sort(_) => ("view-sort-ascending", color::text_tertiary()),
    };
    let slot = Length::Fixed(menu::MARK);
    container(parts::mark(crate::icons::symbolic(name), menu::MARK, tint))
        .width(slot)
        .height(slot)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Rectangle, Size};

    fn entry(name: &str, kind: Kind) -> Entry {
        Entry {
            name: name.to_owned(),
            path: PathBuf::from("/d").join(name),
            kind,
            mime: None,
            modified: None,
            size: 0,
            to_dir: false,
            broken: false,
            launcher: None,
        }
    }

    #[test]
    fn a_missing_icon_says_what_it_is_not_what_it_is_called() {
        assert_eq!(badge(&entry("photo.png", Kind::File)), "PNG");
        assert_eq!(badge(&entry("notes", Kind::File)), "FILE");
        assert_eq!(badge(&entry(".bashrc", Kind::File)), "FILE");
        assert_eq!(badge(&entry("a.verylongext", Kind::File)), "FILE");
        assert_eq!(badge(&entry("x", Kind::Dir)), "DIR");
        assert_eq!(badge(&entry("x.desktop", Kind::Launcher)), "APP");
        let mut l = entry("l", Kind::Symlink);
        l.broken = true;
        assert_eq!(badge(&l), "LINK");
    }

    #[test]
    fn the_prompt_hangs_under_its_icon_and_flips_above_at_the_bottom() {
        let size = Size::new(1000.0, 600.0);
        let top = Rectangle::new(
            Point::new(14.0, 14.0),
            Size::new(desktop::CELL_W, desktop::CELL_H),
        );
        let p = prompt_origin(top, size);
        assert!(p.y > top.y + top.height);
        assert!(p.x >= desktop::MARGIN);
        let low = Rectangle::new(
            Point::new(900.0, 480.0),
            Size::new(desktop::CELL_W, desktop::CELL_H),
        );
        let q = prompt_origin(low, size);
        assert!(q.y + desktop::PROMPT_H <= low.y);
        assert!(q.x + desktop::PROMPT_W <= size.width);
    }
}
