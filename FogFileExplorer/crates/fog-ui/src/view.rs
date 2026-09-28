// SPDX-License-Identifier: AGPL-3.0-only

//! Drawing (FOG §UI and navigation).
//!
//! Composition: the listing is the hero, a dense sortable table that takes
//! every spare pixel. Around it the chrome changes silhouette at every step
//! so nothing reads as a stack of panels: a rail of places on the left, then
//! top to bottom a strip of tabs (only with more than one), the breadcrumb
//! bar, a caption-height column header, the table, the job tray (only while
//! there are jobs: a lower band of meters, each row underlined by its
//! progress), and a one-line status. The conflict and delete dialogs are
//! centred boxes over a scrim, like the palette.
//!
//! One gold value, always: the cursor of whichever region has the keyboard
//! (the list's row, the sidebar's place, the tray's job, the palette's or a
//! dialog's pick, or the caret of the path or name being typed). Every other
//! cursor, every mark and every progress bar goes neutral.

use std::time::{Duration, Instant};

use fog_config::{Action, Target};
use fog_proto::{Entry, JobStatus, Kind, SortKey, StatReply, TrashItem};
use fog_widgets::virtual_list;
use iced::widget::text::Wrapping;
use iced::widget::{
    column, container, mouse_area, opaque, responsive, row, stack, text, Column, Row, Space,
};
use iced::{alignment, Background, Color, Element, Length, Size};
use jiff::tz::TimeZone;
use jiff::Timestamp;

use crate::app::{list_id, App, Focus, Fogd, JobCtl, Message, Overlay, SECTIONS};
use crate::clip::ClipOp;
use crate::edit::LineEdit;
use crate::ops::{resolution_label, Confirm, Conflict, Job, JobKind, NameEntry, NameFor};
use crate::palette::{Item, Palette};
use crate::state::{join, split_parent, Browser, Notice};
use crate::theme::{color, size};

pub fn view(app: &App) -> Element<'_, Message> {
    let b = app.tabs.active();
    let mut main = Column::new();
    if app.tabs.len() > 1 {
        main = main.push(tab_strip(app));
    }
    main = main.push(chrome(path_bar(app), Edge::Bottom)).push(
        responsive(move |room: Size| {
            // iced's embedded scrollbar only takes width while the list
            // overflows; the header's labels follow it.
            let overflow = b.len() as f32 * size::ROW_H > room.height - size::HEADER_H;
            column![
                header(app, overflow),
                container(list(app)).height(Length::Fill)
            ]
            .into()
        })
        .height(Length::Fill),
    );
    if !app.tray.jobs.is_empty() {
        main = main.push(tray(app));
    }
    main = main.push(chrome(status_line(app, b), Edge::Top));

    let mut body = Row::new();
    if app.sidebar {
        body = body.push(sidebar(app));
    }
    let base = container(body.push(main))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| ground(color::BASE, Some(color::TEXT)));

    // A waiting conflict outranks any overlay: its job is stalled on it.
    if let Some(c) = app.conflicts.first() {
        let content = conflict_dialog(app, c);
        return modal(base, content, None);
    }
    match &app.overlay {
        Overlay::Palette(p) => stack![
            base,
            scrim(Some(Message::Dismiss)),
            container(opaque(palette(app, p)))
                .width(Length::Fill)
                .padding([size::PALETTE_TOP, 0.0])
                .align_x(alignment::Horizontal::Center),
        ]
        .into(),
        Overlay::Confirm(c) => modal(base, confirm_dialog(c), Some(Message::Confirm(false))),
        _ => base.into(),
    }
}

/// The dimmed window under an overlay; a click on it sends `dismiss`.
fn scrim<'a>(dismiss: Option<Message>) -> Element<'a, Message> {
    let s = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| ground(color::SCRIM, None)),
    );
    match dismiss {
        Some(m) => s.on_press(m).into(),
        None => s.into(),
    }
}

/// A dialog centred over the scrim.
fn modal<'a>(
    base: impl Into<Element<'a, Message>>,
    dialog: Element<'a, Message>,
    dismiss: Option<Message>,
) -> Element<'a, Message> {
    stack![
        base.into(),
        scrim(dismiss),
        container(opaque(dialog)).center(Length::Fill),
    ]
    .into()
}

/// The hard-edged box of the palette and the dialogs.
fn boxed(_: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(color::CHROME)),
        border: iced::Border {
            color: color::RULE,
            width: size::HAIRLINE,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn ground(bg: Color, fg: Option<Color>) -> container::Style {
    container::Style {
        background: Some(Background::Color(bg)),
        text_color: fg,
        ..Default::default()
    }
}

fn label<'a>(s: impl text::IntoFragment<'a>, sz: f32, c: Color) -> text::Text<'a> {
    text(s).size(sz).color(c).wrapping(Wrapping::None)
}

/// A filled block: the accent bar, rules, the caret.
fn block<'a>(w: impl Into<Length>, h: impl Into<Length>, c: Option<Color>) -> Element<'a, Message> {
    container(Space::new())
        .width(w)
        .height(h)
        .style(move |_| container::Style {
            background: c.map(Background::Color),
            ..Default::default()
        })
        .into()
}

fn rule_h<'a>() -> Element<'a, Message> {
    block(Length::Fill, size::HAIRLINE, Some(color::RULE))
}

fn rule_v<'a>() -> Element<'a, Message> {
    block(size::HAIRLINE, Length::Fill, Some(color::RULE))
}

#[derive(Clone, Copy)]
enum Edge {
    Top,
    Bottom,
}

/// A thin chrome row: lifted ground, hairline rule on the edge that meets
/// the listing.
fn chrome(content: Element<'_, Message>, edge: Edge) -> Element<'_, Message> {
    let body = container(content)
        .padding([size::CHROME_Y, size::PAD_X])
        .width(Length::Fill)
        .style(|_| ground(color::CHROME, None));
    match edge {
        Edge::Top => column![rule_h(), body].into(),
        Edge::Bottom => column![body, rule_h()].into(),
    }
}

/// Nothing modal is open: the regions' own cursors may take the gold.
fn unobstructed(app: &App) -> bool {
    matches!(app.overlay, Overlay::None) && app.conflicts.is_empty()
}

/// Which cursor is the gold one.
fn list_focused(app: &App) -> bool {
    app.focus == Focus::List && unobstructed(app)
}

// ---------------------------------------------------------------- tabs

/// Tabs, only when there are two or more: a lower ground than the chrome,
/// the active tab opening onto the path bar in the chrome's own colour.
fn tab_strip(app: &App) -> Element<'_, Message> {
    let mut strip = Row::new().height(size::TAB_H);
    for (i, b) in app.tabs.iter().enumerate() {
        let on = i == app.tabs.index();
        let name = basename(b.target());
        let tab = container(label(
            name,
            size::TEXT_SMALL,
            if on {
                color::TEXT
            } else {
                color::TEXT_TERTIARY
            },
        ))
        .padding([0.0, size::PAD_X])
        .max_width(size::TAB_MAX_W)
        .height(Length::Fill)
        .align_y(alignment::Vertical::Center)
        .clip(true)
        .style(move |_| {
            ground(
                if on {
                    color::TAB_ACTIVE
                } else {
                    color::TAB_STRIP
                },
                None,
            )
        });
        strip = strip
            .push(mouse_area(tab).on_press(Message::Tab(i)))
            .push(rule_v());
    }
    column![
        container(strip)
            .width(Length::Fill)
            .style(|_| ground(color::TAB_STRIP, None)),
        rule_h(),
    ]
    .into()
}

/// A tray row's subject: the path being worked on, else the first item the
/// job named and how many more. An undo says what it undoes, as far as the
/// tray knows.
fn job_subject(j: &Job) -> String {
    if !j.current.is_empty() {
        return basename(&j.current);
    }
    let mut s = if j.subject.is_empty() {
        String::new()
    } else {
        basename(&j.subject)
    };
    if j.count > 1 {
        s.push_str(&format!(" +{}", group(j.count - 1)));
    }
    match (j.kind, j.undoes) {
        (JobKind::Undo, Some(k)) => format!("undo {} {s}", k.label()),
        (JobKind::Undo, None) => "undo last change".to_owned(),
        (k, _) if s.is_empty() => k.label().to_owned(),
        _ => s,
    }
}

pub fn basename(path: &[u8]) -> String {
    match path.iter().rposition(|&c| c == b'/') {
        Some(i) if i + 1 < path.len() => String::from_utf8_lossy(&path[i + 1..]).into_owned(),
        _ => "/".to_owned(),
    }
}

// ---------------------------------------------------------------- sidebar

/// The places rail: sections by kind, the current folder's place neutral,
/// the sidebar's own cursor gold only while it has the keyboard.
fn sidebar(app: &App) -> Element<'_, Message> {
    let here = app.tabs.active().target();
    let focused = app.focus == Focus::Places && unobstructed(app);
    let mut col = Column::new();
    for (title, kinds) in SECTIONS {
        let members: Vec<usize> = (0..app.places.len())
            .filter(|&i| kinds.contains(&app.places[i].kind))
            .collect();
        if members.is_empty() {
            continue;
        }
        col = col.push(block(Length::Fill, size::SECTION_GAP, None));
        if !title.is_empty() {
            col = col.push(
                container(label(title, size::TEXT_SMALL, color::TEXT_TERTIARY))
                    .padding([0.0, size::PAD_X])
                    .height(size::HEADER_H)
                    .align_y(alignment::Vertical::Center),
            );
        }
        for i in members {
            let p = &app.places[i];
            let cursor = focused && i == app.place_sel;
            let current = p.path == here;
            col = col.push(
                mouse_area(place_row(&p.label, cursor, current))
                    .on_press(Message::Go(p.path.clone())),
            );
        }
    }
    if app.places.is_empty() {
        col = col.push(block(Length::Fill, size::SECTION_GAP, None)).push(
            container(label("no places", size::TEXT_SMALL, color::TEXT_TERTIARY))
                .padding([0.0, size::PAD_X]),
        );
    }
    row![
        container(col)
            .width(size::SIDEBAR_W)
            .height(Length::Fill)
            .clip(true)
            .style(|_| ground(color::CHROME, None)),
        rule_v(),
    ]
    .into()
}

fn place_row(name: &str, cursor: bool, current: bool) -> Element<'static, Message> {
    let (bar, fill, fg) = match (cursor, current) {
        (true, _) => (
            Some(color::ACCENT),
            Some(color::ACCENT_FILL),
            color::ACCENT_TEXT,
        ),
        (false, true) => (Some(color::NEUTRAL), Some(color::MARK_FILL), color::TEXT),
        (false, false) => (None, None, color::TEXT_SECONDARY),
    };
    container(row![
        block(size::BAR_W, Length::Fill, bar),
        container(label(name.to_owned(), size::TEXT, fg))
            .padding([0.0, size::PAD_X - size::BAR_W])
            .height(Length::Fill)
            .align_y(alignment::Vertical::Center)
            .clip(true),
    ])
    .width(Length::Fill)
    .height(size::ROW_H)
    .style(move |_| container::Style {
        background: fill.map(Background::Color),
        ..Default::default()
    })
    .into()
}

// ---------------------------------------------------------------- path bar

/// Breadcrumbs, or the path editor while it is open.
fn path_bar(app: &App) -> Element<'_, Message> {
    if let Overlay::Path(p) = &app.overlay {
        let ghost = p.ghost(app.tabs.active().show_hidden).unwrap_or_default();
        return line_edit(&p.line, color::ACCENT, ghost);
    }
    let path = app.tabs.active().target().to_vec();
    let crumbs = crumb_bar(path);
    if !app.in_trash() {
        return crumbs;
    }
    // In the trash the bar carries what can be done there.
    let act = |name: &'static str, a: Action| {
        mouse_area(row![
            label(name, size::TEXT_SMALL, color::TEXT_SECONDARY),
            label(
                format!(" {}", chord_of(app, Target::Action(a))),
                size::TEXT_SMALL,
                color::TEXT_TERTIARY
            ),
        ])
        .on_press(Message::Act(a))
    };
    row![
        container(crumbs).width(Length::Fill),
        row![
            act("restore", Action::Restore),
            act("delete permanently", Action::Delete)
        ]
        .spacing(size::GAP),
    ]
    .spacing(size::GAP)
    .align_y(alignment::Vertical::Center)
    .into()
}

fn crumb_bar<'a>(path: Vec<u8>) -> Element<'a, Message> {
    responsive(move |room: Size| {
        // The UI font is monospace, so a width is a character count.
        let fits = (room.width / (size::TEXT * size::MONO_ADVANCE)) as usize;
        let parts = crumbs(&path, fits);
        let last = parts.len().saturating_sub(1);
        let mut r = Row::new();
        for (i, (s, to)) in parts.into_iter().enumerate() {
            let fg = if i == last {
                color::TEXT
            } else {
                color::TEXT_TERTIARY
            };
            let l = label(s, size::TEXT, fg);
            r = match to {
                Some(to) if i != last => r.push(mouse_area(l).on_press(Message::Go(to))),
                _ => r.push(l),
            };
        }
        container(r).width(Length::Fill).clip(true).into()
    })
    .height(Length::Shrink)
    .into()
}

/// One editable line: text, a caret in `caret`'s colour, then `ghost` (the
/// completion Tab would take) in tertiary.
fn line_edit<'a>(line: &LineEdit, caret: Color, ghost: String) -> Element<'a, Message> {
    let (before, after) = line.halves();
    row![
        label(before.to_owned(), size::TEXT, color::TEXT),
        block(size::CARET_W, size::TEXT, Some(caret)),
        label(after.to_owned(), size::TEXT, color::TEXT),
        label(ghost, size::TEXT, color::TEXT_TERTIARY),
    ]
    .align_y(alignment::Vertical::Center)
    .into()
}

/// `path` as breadcrumbs, the root first: each segment's text (ancestors
/// keep their trailing `/`) and the folder a click opens. Too wide for
/// `fits` characters, the leading crumbs give way to one `…/`; if even the
/// current folder's name does not fit, its own tail is kept.
pub fn crumbs(path: &[u8], fits: usize) -> Vec<(String, Option<Vec<u8>>)> {
    let parts: Vec<&[u8]> = path
        .split(|&c| c == b'/')
        .filter(|p| !p.is_empty())
        .collect();
    let mut out = vec![("/".to_owned(), Some(b"/".to_vec()))];
    let mut at = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        at.push(b'/');
        at.extend_from_slice(p);
        let mut s = String::from_utf8_lossy(p).into_owned();
        if i + 1 < parts.len() {
            s.push('/');
        }
        out.push((s, Some(at.clone())));
    }
    let width =
        |v: &[(String, Option<Vec<u8>>)]| -> usize { v.iter().map(|c| c.0.chars().count()).sum() };
    if out.len() == 1 || width(&out) <= fits {
        return out;
    }
    const ELLIPSIS: &str = "…/";
    let last = out.len() - 1;
    let start = (1..last)
        .find(|&s| ELLIPSIS.chars().count() + width(&out[s..]) <= fits)
        .unwrap_or(last);
    if ELLIPSIS.chars().count() + width(&out[start..]) <= fits {
        let mut v = vec![(ELLIPSIS.to_owned(), None)];
        v.extend(out.drain(start..));
        return v;
    }
    let (name, to) = out.pop().unwrap_or_default();
    let n = name.chars().count();
    let keep = fits.saturating_sub(1);
    let tail: String = name.chars().skip(n.saturating_sub(keep)).collect();
    vec![(format!("…{tail}"), to)]
}

// ---------------------------------------------------------------- header

/// Caption-height column labels over the table, or the path editor's
/// completions while it is open. Clicking a label sorts by it (in fogd).
fn header(app: &App, scrollbar: bool) -> Element<'_, Message> {
    let b = app.tabs.active();
    let content: Element<'_, Message> = match &app.overlay {
        Overlay::Path(p) => {
            let cands = p.candidates(b.show_hidden);
            let mut r = Row::new().spacing(size::GAP);
            if cands.is_empty() {
                r = r.push(label("no folders", size::TEXT_SMALL, color::TEXT_TERTIARY));
            }
            for (i, e) in cands.iter().enumerate() {
                let fg = if i == p.pick % cands.len().max(1) {
                    color::TEXT
                } else {
                    color::TEXT_TERTIARY
                };
                r = r.push(label(format!("{}/", e.display()), size::TEXT_SMALL, fg));
            }
            container(r)
                .padding([0.0, size::PAD_X])
                .width(Length::Fill)
                .clip(true)
                .into()
        }
        Overlay::Name(
            n @ NameEntry {
                what: NameFor::Folder | NameFor::File,
                ..
            },
        ) => {
            let what = if n.what == NameFor::Folder {
                "new folder  "
            } else {
                "new file  "
            };
            let mut r = row![
                label(what, size::TEXT_SMALL, color::TEXT_TERTIARY),
                line_edit(&n.line, color::ACCENT, String::new()),
            ]
            .align_y(alignment::Vertical::Center);
            if let Some(e) = n.error {
                r = r.push(label(format!("  {e}"), size::TEXT_SMALL, color::DANGER));
            }
            container(r)
                .padding([0.0, size::PAD_X])
                .width(Length::Fill)
                .clip(true)
                .into()
        }
        _ => {
            let s = b.sort;
            let trash = app.in_trash();
            let col = |name: &'static str, key: SortKey| {
                let on = s.key == key;
                let arrow = match (on, s.reverse) {
                    (false, _) => "",
                    (true, false) => " ↑",
                    (true, true) => " ↓",
                };
                label(
                    format!("{name}{arrow}"),
                    size::TEXT_SMALL,
                    if on {
                        color::TEXT
                    } else {
                        color::TEXT_TERTIARY
                    },
                )
            };
            let right = |t: text::Text<'static>, w: f32, key| {
                mouse_area(t.width(w).align_x(alignment::Horizontal::Right))
                    .on_press(Message::Header(key))
            };
            let mut r = row![mouse_area(
                container(col("name", SortKey::Name))
                    .padding([0.0, size::PAD_X])
                    .width(Length::Fill)
            )
            .on_press(Message::Header(SortKey::Name))];
            if trash {
                r = r.push(
                    label("from", size::TEXT_SMALL, color::TEXT_TERTIARY)
                        .width(size::FROM_W)
                        .align_x(alignment::Horizontal::Left),
                );
            }
            r = r
                .push(right(
                    col("size", SortKey::Size),
                    size::SIZE_W,
                    SortKey::Size,
                ))
                .push(right(
                    // fogd sorts the trash by the files' own times: the
                    // label says what the column shows.
                    col(
                        if trash { "deleted" } else { "modified" },
                        SortKey::Modified,
                    ),
                    size::DATE_W,
                    SortKey::Modified,
                ));
            if !trash {
                r = r.push(right(
                    col("type", SortKey::Type),
                    size::TYPE_W,
                    SortKey::Type,
                ));
            }
            r.push(block(
                size::PAD_X - size::BAR_W + if scrollbar { size::SCROLLBAR_W } else { 0.0 },
                size::HAIRLINE,
                None,
            ))
            .into()
        }
    };
    column![
        container(content)
            .height(size::HEADER_H)
            .width(Length::Fill)
            .align_y(alignment::Vertical::Center),
        rule_h(),
    ]
    .into()
}

// ---------------------------------------------------------------- list

fn list(app: &App) -> Element<'_, Message> {
    let b = app.tabs.active();
    if b.len() == 0 && b.complete && b.pending.is_none() {
        // An empty folder, or a filter with no match, says so quietly.
        let msg = if b.filter.is_empty() {
            "empty folder".to_owned()
        } else {
            format!("nothing matches “{}”", b.filter)
        };
        return container(label(msg, size::TEXT_SMALL, color::TEXT_TERTIARY))
            .center(Length::Fill)
            .into();
    }
    let focused = list_focused(app);
    let tz = &app.tz;
    let renaming = match &app.overlay {
        Overlay::Name(NameEntry {
            what: NameFor::Rename { path, .. },
            line,
            ..
        }) => Some((path.as_slice(), line)),
        _ => None,
    };
    let trash = app.in_trash().then_some(&app.trash);
    virtual_list(b.len(), size::ROW_H, move |i| {
        let cursor = match (i == b.selected, focused) {
            (false, _) => Cursor::No,
            (true, true) => Cursor::Focused,
            (true, false) => Cursor::Idle,
        };
        let entry = b.row(i);
        let path = || entry.map(|e| join(&b.path, &e.name)).unwrap_or_default();
        let extra = Extra {
            edit: renaming.filter(|(p, _)| *p == path()).map(|(_, l)| l),
            trash: trash.map(|t| t.get(&path())),
        };
        let r = list_row(entry, cursor, b.is_marked(i), tz, extra);
        mouse_area(r)
            .on_press(Message::Row(i))
            .on_double_click(Message::OpenRow(i))
            .into()
    })
    .id(list_id())
    .into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cursor {
    No,
    /// The cursor, with the keyboard here: the pane's gold value.
    Focused,
    /// The cursor while another region has the keyboard.
    Idle,
}

/// What a row shows beyond its entry.
struct Extra<'a> {
    /// The name being typed, while this row is renamed.
    edit: Option<&'a LineEdit>,
    /// In the trash view: the item's record, if `ListTrash` named it.
    trash: Option<Option<&'a TrashItem>>,
}

/// One row: `[bar] name[/] … size modified type`. Folders read primary,
/// files secondary; metadata still on its way from fogd reads `·`. In the
/// trash a row is named by where it came from: `name from size deleted`.
fn list_row(
    entry: Option<&Entry>,
    cursor: Cursor,
    marked: bool,
    tz: &TimeZone,
    extra: Extra<'_>,
) -> Element<'static, Message> {
    let Some(entry) = entry else {
        return Space::new().into();
    };
    let is_dir = entry.kind == Kind::Dir;
    let name_color = match (cursor, is_dir || marked) {
        (Cursor::Focused, _) => color::ACCENT_TEXT,
        (_, true) => color::TEXT,
        (_, false) => color::TEXT_SECONDARY,
    };
    let (bar, fill) = match (cursor, marked) {
        (Cursor::Focused, _) => (Some(color::ACCENT), Some(color::ACCENT_FILL)),
        (Cursor::Idle, _) => (Some(color::NEUTRAL), Some(color::MARK_FILL)),
        (Cursor::No, true) => (None, Some(color::MARK_FILL)),
        (Cursor::No, false) => (None, None),
    };
    let item = extra.trash.flatten();
    let shown = match item {
        Some(t) => basename(&t.original_path),
        None => entry.display().into_owned(),
    };
    let mut name: Row<'static, Message> = match extra.edit {
        // The caret is the gold while a name is typed; the row idles.
        Some(line) => row![line_edit(line, color::ACCENT, String::new())],
        None => row![label(shown, size::TEXT, name_color)],
    };
    if is_dir {
        name = name.push(label("/", size::TEXT, color::TEXT_TERTIARY));
    }
    let data = |s: String, w: f32| {
        label(s, size::TEXT_SMALL, color::TEXT_TERTIARY)
            .width(w)
            .align_x(alignment::Horizontal::Right)
    };
    let mut body = row![container(name).width(Length::Fill).clip(true)];
    match extra.trash {
        Some(item) => {
            let from = item
                .and_then(|t| split_parent(&t.original_path))
                // Elided from the left, like the path bar: the tail is what
                // tells two trashed items' folders apart.
                .map(|(dir, _)| {
                    let fits = (size::FROM_W / (size::TEXT_SMALL * size::MONO_ADVANCE)) as usize;
                    crumbs(&dir, fits).into_iter().map(|(t, _)| t).collect()
                })
                .unwrap_or_else(|| PENDING.to_owned());
            let deleted = item
                .and_then(|t| t.deleted_s)
                .map(|s| i128::from(s) * 1_000_000_000);
            body = body
                .push(
                    container(label(from, size::TEXT_SMALL, color::TEXT_TERTIARY))
                        .width(size::FROM_W)
                        .clip(true),
                )
                .push(data(size_text(entry), size::SIZE_W))
                .push(data(date_text(deleted, tz), size::DATE_W));
        }
        None => {
            let kind = entry.type_label();
            body = body
                .push(data(size_text(entry), size::SIZE_W))
                .push(data(date_text(entry.mtime_ns, tz), size::DATE_W))
                .push(data(
                    if kind.is_empty() {
                        PENDING.to_owned()
                    } else {
                        kind.into_owned()
                    },
                    size::TYPE_W,
                ));
        }
    }
    let body = body
        .align_y(alignment::Vertical::Center)
        .height(Length::Fill);
    container(row![
        block(size::BAR_W, Length::Fill, bar),
        container(body)
            .padding([0.0, size::PAD_X - size::BAR_W])
            .width(Length::Fill)
            .height(Length::Fill),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| container::Style {
        background: fill.map(Background::Color),
        ..Default::default()
    })
    .into()
}

/// Metadata fogd has not sent yet.
const PENDING: &str = "·";

fn size_text(e: &Entry) -> String {
    match (e.kind, e.size) {
        (Kind::Dir, _) => String::new(),
        (_, None) => PENDING.to_owned(),
        (_, Some(n)) => bytes(n),
    }
}

fn date_text(mtime_ns: Option<i128>, tz: &TimeZone) -> String {
    let Some(ns) = mtime_ns else {
        return PENDING.to_owned();
    };
    match Timestamp::from_nanosecond(ns) {
        Ok(t) => t
            .to_zoned(tz.clone())
            .strftime("%Y-%m-%d %H:%M")
            .to_string(),
        Err(_) => PENDING.to_owned(),
    }
}

/// Binary units: `812 B`, `4.0 KiB`, `12 KiB`, `1.5 GiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{:.0} {}", v.floor(), UNITS[u])
    }
}

// ---------------------------------------------------------------- status

fn status_line<'a>(app: &'a App, b: &'a Browser) -> Element<'a, Message> {
    let mut left = Row::new().spacing(size::GAP);
    let count = if b.filter.is_empty() {
        items(b.len())
    } else {
        format!("{} of {}", group(b.len()), items(b.unfiltered()))
    };
    left = left.push(label(count, size::TEXT_SMALL, color::TEXT_SECONDARY));
    let hidden = b.total() - b.unfiltered();
    if hidden > 0 {
        left = left.push(label(
            format!("{} hidden", group(hidden)),
            size::TEXT_SMALL,
            color::TEXT_TERTIARY,
        ));
    }
    if !b.filter.is_empty() {
        left = left.push(row![
            label("filter ", size::TEXT_SMALL, color::TEXT_TERTIARY),
            label(b.filter.clone(), size::TEXT_SMALL, color::TEXT),
        ]);
    }
    let sel = b.selection();
    if !sel.is_empty() {
        left = left.push(label(
            match b.selected_bytes() {
                Some(n) => format!("{} selected · {}", group(sel.len()), bytes(n)),
                None => format!("{} selected", group(sel.len())),
            },
            size::TEXT_SMALL,
            color::TEXT,
        ));
    }
    if b.pending.is_some() || !b.complete {
        left = left.push(label("listing…", size::TEXT_SMALL, color::TEXT_TERTIARY));
    }
    if let Overlay::Name(n) = &app.overlay {
        let rename = matches!(n.what, NameFor::Rename { .. });
        // A new name's error sits in the header beside it; an inline
        // rename has no room in its row, so its error comes here.
        let (s, c) = match n.error {
            Some(e) if rename => (e.to_owned(), color::DANGER),
            _ if rename => ("enter rename · esc cancel".to_owned(), color::TEXT_TERTIARY),
            _ => ("enter create · esc cancel".to_owned(), color::TEXT_TERTIARY),
        };
        left = left.push(label(s, size::TEXT_SMALL, c));
    } else if let Some((path, errno)) = &b.error {
        left = left.push(label(
            format!("{} — {}", strerror(*errno), String::from_utf8_lossy(path)),
            size::TEXT_SMALL,
            color::DANGER,
        ));
    } else if let Some(n) = &b.notice {
        let (s, c) = notice(n);
        left = left.push(label(s, size::TEXT_SMALL, c));
    }
    let mut right = Row::new().spacing(size::GAP);
    if let Some((free, _total)) = b.space {
        right = right.push(label(
            format!("{} free", bytes(free)),
            size::TEXT_SMALL,
            color::TEXT_TERTIARY,
        ));
    }
    let live = app.tray.live();
    right = right.push(label(
        match live {
            1 => "1 job".to_owned(),
            n => format!("{} jobs", group(n)),
        },
        size::TEXT_SMALL,
        if live > 0 {
            color::TEXT_SECONDARY
        } else {
            color::TEXT_TERTIARY
        },
    ));
    if b.len() > 0 {
        right = right.push(label(
            format!("{} / {}", group(b.selected + 1), group(b.len())),
            size::TEXT_SMALL,
            color::TEXT_TERTIARY,
        ));
    }
    let (fogd, fogd_color) = match app.fogd {
        Fogd::Connecting => ("fogd …", color::TEXT_TERTIARY),
        Fogd::Up => ("fogd", color::TEXT_TERTIARY),
        Fogd::Down => ("fogd not running", color::DANGER),
    };
    right = right.push(label(fogd, size::TEXT_SMALL, fogd_color));
    // The left side yields and clips; the right side never does.
    row![container(left).width(Length::Fill).clip(true), right]
        .spacing(size::GAP)
        .into()
}

fn items(n: usize) -> String {
    match n {
        1 => "1 item".to_owned(),
        n => format!("{} items", group(n)),
    }
}

fn strerror(errno: i32) -> String {
    let msg = std::io::Error::from_raw_os_error(errno).to_string();
    msg.split(" (os error").next().unwrap_or(&msg).to_owned()
}

fn notice(n: &Notice) -> (String, Color) {
    let lossy = |p: &[u8]| String::from_utf8_lossy(p).into_owned();
    match n {
        Notice::Opened(p) => (format!("opened {}", basename(p)), color::TEXT_TERTIARY),
        Notice::OpenFailed(p, e) => (
            format!("cannot open {}: {}", lossy(p), strerror(*e)),
            color::DANGER,
        ),
        Notice::Ran(name) => (format!("ran {name}"), color::TEXT_TERTIARY),
        Notice::RunFailed(name, why) => (format!("{name}: {why}"), color::DANGER),
        Notice::Unavailable(a) => (format!("{a}: not in this build"), color::TEXT_TERTIARY),
        Notice::Hint(s) => ((*s).to_owned(), color::TEXT_TERTIARY),
        Notice::Clipboard(op, n) => (
            if *op == ClipOp::Cut {
                format!("{} cut · paste to move", items(*n))
            } else {
                format!("{} on the clipboard", items(*n))
            },
            color::TEXT_SECONDARY,
        ),
        Notice::NothingToPaste => ("nothing to paste".to_owned(), color::TEXT_TERTIARY),
        Notice::Done(k, n) => {
            let verb = match k {
                JobKind::Copy => "copied",
                JobKind::Move => "moved",
                JobKind::Trash => "moved to trash",
                JobKind::Delete => "deleted permanently",
                JobKind::Restore => "restored",
                _ => "done:",
            };
            (format!("{verb} {}", items(*n)), color::TEXT_SECONDARY)
        }
        Notice::Failed(k, msg) => (format!("{} failed: {msg}", k.label()), color::DANGER),
        Notice::Undone => ("undone".to_owned(), color::TEXT_SECONDARY),
        Notice::UndoRefused(why) => (format!("cannot undo: {why}"), color::TEXT_SECONDARY),
    }
}

/// `100002` as `100,002`.
pub fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------- tray

/// The job tray: one meter per job, a lower band than the chrome. Each row
/// reads `kind  file  amount  eta  controls` over a full-width progress
/// bar. The cursor is gold only while the tray has the keyboard.
fn tray(app: &App) -> Element<'_, Message> {
    let focused = app.focus == Focus::Jobs && unobstructed(app);
    let now = Instant::now();
    let jobs = &app.tray.jobs;
    let start = app.tray.cursor.saturating_sub(size::TRAY_ROWS - 1);
    let mut col = Column::new();
    for (i, j) in jobs.iter().enumerate().skip(start).take(size::TRAY_ROWS) {
        col = col.push(job_row(j, focused && i == app.tray.cursor, now));
    }
    let hidden = jobs.len().saturating_sub(size::TRAY_ROWS);
    if hidden > 0 {
        let hint = chord_of(app, Target::Action(Action::FocusJobs));
        col = col.push(
            container(label(
                format!("{} more · {hint}", group(hidden)),
                size::TEXT_SMALL,
                color::TEXT_TERTIARY,
            ))
            .padding([0.0, size::PAD_X])
            .height(size::HEADER_H)
            .align_y(alignment::Vertical::Center),
        );
    }
    column![
        rule_h(),
        container(col)
            .width(Length::Fill)
            .style(|_| ground(color::TRAY, None)),
    ]
    .into()
}

fn job_row(j: &Job, cursor: bool, now: Instant) -> Element<'static, Message> {
    let (bar, fill, kind_fg) = if cursor {
        (
            Some(color::ACCENT),
            Some(color::ACCENT_FILL),
            color::ACCENT_TEXT,
        )
    } else {
        (None, None, color::TEXT_SECONDARY)
    };
    let failed = matches!(j.status, JobStatus::Failed { .. });
    let mut what = job_subject(j);
    if let JobStatus::Failed { msg, .. } = &j.status {
        what = format!("{what} — {msg}");
    }
    let amount = if j.bytes_total > 0 {
        format!("{} / {}", bytes(j.bytes_done), bytes(j.bytes_total))
    } else if j.files_total > 0 {
        format!(
            "{} / {} files",
            group(j.files_done as usize),
            group(j.files_total as usize)
        )
    } else {
        String::new()
    };
    let (state, state_fg) = match &j.status {
        JobStatus::Queued => ("queued".to_owned(), color::TEXT_TERTIARY),
        JobStatus::Running => (
            j.eta(now).map(clock).unwrap_or_default(),
            color::TEXT_TERTIARY,
        ),
        JobStatus::Paused => ("paused".to_owned(), color::TEXT),
        JobStatus::Conflict { .. } => ("waiting".to_owned(), color::TEXT),
        JobStatus::Done => ("done".to_owned(), color::TEXT_TERTIARY),
        JobStatus::Failed { .. } => ("failed".to_owned(), color::DANGER),
        JobStatus::Cancelled => ("cancelled".to_owned(), color::TEXT_TERTIARY),
    };
    let ctl = |key: &'static str, word: &'static str, c: JobCtl| {
        mouse_area(row![
            label(key, size::TEXT_SMALL, color::TEXT_TERTIARY),
            label(format!(" {word}"), size::TEXT_SMALL, color::TEXT_SECONDARY),
        ])
        .on_press(Message::Job(j.id, c))
    };
    let mut controls = Row::new().spacing(size::GAP);
    if j.live() {
        let pause = if j.status == JobStatus::Paused {
            "resume"
        } else {
            "pause"
        };
        controls =
            controls
                .push(ctl("p", pause, JobCtl::Pause))
                .push(ctl("c", "cancel", JobCtl::Cancel));
    } else {
        controls = controls.push(ctl("x", "dismiss", JobCtl::Dismiss));
    }
    let right = |s: String, w: f32, c: Color| {
        label(s, size::TEXT_SMALL, c)
            .width(w)
            .align_x(alignment::Horizontal::Right)
    };
    let body = row![
        label(j.kind.label(), size::TEXT_SMALL, kind_fg).width(size::KIND_W),
        container(label(
            what,
            size::TEXT,
            if failed { color::DANGER } else { color::TEXT }
        ))
        .width(Length::Fill)
        .clip(true),
        right(amount, size::AMOUNT_W, color::TEXT_SECONDARY),
        right(state, size::ETA_W, state_fg),
        container(controls).padding([0.0, size::GAP]),
    ]
    .align_y(alignment::Vertical::Center)
    .height(Length::Fill);
    let line = container(row![
        block(size::BAR_W, Length::Fill, bar),
        container(body)
            .padding([0.0, size::PAD_X - size::BAR_W])
            .width(Length::Fill)
            .height(Length::Fill),
    ])
    .width(Length::Fill)
    .height(size::ROW_H)
    .style(move |_| container::Style {
        background: fill.map(Background::Color),
        ..Default::default()
    });
    column![line, progress(j.fraction(), failed)].into()
}

/// A full-width meter: the done part over the track.
fn progress<'a>(fraction: Option<f32>, failed: bool) -> Element<'a, Message> {
    const STEPS: f32 = 1000.0;
    let done = (fraction.unwrap_or(0.0).clamp(0.0, 1.0) * STEPS) as u16;
    let rest = STEPS as u16 - done;
    let tone = if failed {
        color::DANGER
    } else {
        color::PROGRESS
    };
    let mut r = Row::new().height(size::PROGRESS_H);
    if done > 0 {
        r = r.push(block(
            Length::FillPortion(done),
            size::PROGRESS_H,
            Some(tone),
        ));
    }
    if rest > 0 {
        r = r.push(block(
            Length::FillPortion(rest),
            size::PROGRESS_H,
            Some(color::TRACK),
        ));
    }
    r.into()
}

/// `0:42`, `12:05`, `1:02:03`.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02} left", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02} left", s / 60, s % 60)
    }
}

// ---------------------------------------------------------------- dialogs

/// One of a dialog's choices: `key word`, the pick gold. Choices share
/// the dialog's width equally.
fn choice(
    key: &'static str,
    word: &'static str,
    on: bool,
    fg: Color,
    press: Message,
) -> Element<'static, Message> {
    let (bar, fill, fg) = if on {
        (
            Some(color::ACCENT),
            Some(color::ACCENT_FILL),
            if fg == color::DANGER {
                fg
            } else {
                color::ACCENT_TEXT
            },
        )
    } else {
        (None, None, fg)
    };
    let mut words = Row::new();
    if !key.is_empty() {
        words = words.push(label(
            format!("{key} "),
            size::TEXT_SMALL,
            color::TEXT_TERTIARY,
        ));
    }
    words = words.push(label(word, size::TEXT, fg));
    let cell = container(row![
        block(size::BAR_W, Length::Fill, bar),
        container(words.align_y(alignment::Vertical::Center))
            .padding([0.0, size::PAD_X - size::BAR_W])
            .height(Length::Fill)
            .align_y(alignment::Vertical::Center),
    ])
    .width(Length::Fill)
    .height(size::ROW_H)
    .style(move |_| container::Style {
        background: fill.map(Background::Color),
        ..Default::default()
    });
    mouse_area(cell).on_press(press).into()
}

/// Chrome padding around a dialog section.
fn section<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding([size::CHROME_Y, size::PAD_X])
        .width(Length::Fill)
        .into()
}

/// A job found its destination taken: the two files side by side, then
/// the choices. Skip is picked first; `a` applies the answer to the rest
/// of this job's conflicts.
fn conflict_dialog<'a>(app: &'a App, c: &'a Conflict) -> Element<'a, Message> {
    let lossy = |p: Vec<u8>| String::from_utf8_lossy(&p).into_owned();
    let dir = |p: &[u8]| split_parent(p).map(|(d, _)| lossy(d)).unwrap_or_default();
    let waiting = app.conflicts.len();
    let head = column![
        row![
            container(label(
                format!("“{}” already exists", basename(&c.dest)),
                size::TEXT,
                color::TEXT
            ))
            .width(Length::Fill)
            .clip(true),
            label(
                if waiting > 1 {
                    format!("1 of {}", group(waiting))
                } else {
                    String::new()
                },
                size::TEXT_SMALL,
                color::TEXT_TERTIARY
            ),
        ],
        container(label(
            format!("in {}", dir(&c.dest)),
            size::TEXT_SMALL,
            color::TEXT_TERTIARY
        ))
        .clip(true),
    ];
    let tz = &app.tz;
    let side =
        |title: &'static str, path: &[u8], st: Option<&StatReply>, other: Option<&StatReply>| {
            let files = |s: &StatReply| s.kind != Kind::Dir;
            let (size_s, date) = match st {
                Some(s) if s.kind == Kind::Dir => {
                    ("folder".to_owned(), date_text(Some(s.mtime_ns), tz))
                }
                Some(s) => (bytes(s.size), date_text(Some(s.mtime_ns), tz)),
                None => (PENDING.to_owned(), PENDING.to_owned()),
            };
            let (larger, newer) = match (st, other) {
                (Some(a), Some(b)) => (
                    files(a) && files(b) && a.size > b.size,
                    a.mtime_ns > b.mtime_ns,
                ),
                _ => (false, false),
            };
            let fact = |v: String, tag: bool, word: &'static str| {
                row![
                    label(v, size::TEXT_SMALL, color::TEXT_SECONDARY),
                    label(
                        if tag {
                            format!("  {word}")
                        } else {
                            String::new()
                        },
                        size::TEXT_SMALL,
                        color::TEXT
                    ),
                ]
            };
            container(column![
                label(title, size::TEXT_SMALL, color::TEXT_TERTIARY),
                // The header names the full folder; each side needs only its
                // own folder's name to tell the two apart.
                container(label(
                    format!("{}/", basename(dir(path).as_bytes())),
                    size::TEXT_SMALL,
                    color::TEXT_SECONDARY
                ))
                .clip(true),
                fact(size_s, larger, "larger"),
                fact(date, newer, "newer"),
            ])
            .width(Length::Fill)
        };
    let sides = row![
        side(
            "existing",
            &c.dest,
            c.dest_stat.as_ref(),
            c.src_stat.as_ref()
        ),
        side(
            "incoming",
            &c.src,
            c.src_stat.as_ref(),
            c.dest_stat.as_ref()
        ),
    ]
    .spacing(size::GAP);
    let mut choices = Row::new();
    for r in c.choices() {
        choices = choices.push(choice(
            resolution_key(r),
            resolution_label(r),
            r == c.pick,
            color::TEXT_SECONDARY,
            Message::Resolve(r),
        ));
    }
    let footer = row![
        mouse_area(row![
            label("a ", size::TEXT_SMALL, color::TEXT_TERTIARY),
            label(
                if c.apply_all {
                    "apply to all: on"
                } else {
                    "apply to all: off"
                },
                size::TEXT_SMALL,
                if c.apply_all {
                    color::TEXT
                } else {
                    color::TEXT_SECONDARY
                }
            ),
        ])
        .on_press(Message::ApplyAll),
        Space::new().width(Length::Fill),
        label(
            "enter choose · esc skip",
            size::TEXT_SMALL,
            color::TEXT_TERTIARY
        ),
    ];
    container(column![
        section(head),
        rule_h(),
        section(sides),
        rule_h(),
        choices,
        rule_h(),
        section(footer),
    ])
    .width(size::DIALOG_W)
    .style(boxed)
    .into()
}

/// The letter that picks `r` in the conflict dialog.
fn resolution_key(r: fog_proto::Resolution) -> &'static str {
    use fog_proto::Resolution as R;
    match r {
        R::Replace => "r",
        R::Skip => "s",
        R::KeepBoth => "k",
        R::Merge => "m",
    }
}

/// Permanent delete: what goes, that it cannot come back, and Cancel
/// picked until the user moves to Delete.
fn confirm_dialog(c: &Confirm) -> Element<'_, Message> {
    let head = column![
        label(c.headline(), size::TEXT, color::TEXT),
        container(label(c.listing(), size::TEXT_SMALL, color::TEXT_SECONDARY)).clip(true),
        label(
            "this cannot be undone",
            size::TEXT_SMALL,
            color::TEXT_TERTIARY
        ),
    ];
    let choices = row![
        choice(
            "esc",
            "cancel",
            !c.delete,
            color::TEXT_SECONDARY,
            Message::Confirm(false)
        ),
        choice(
            "",
            "delete permanently",
            c.delete,
            color::DANGER,
            Message::Confirm(true)
        ),
    ];
    container(column![
        section(head),
        rule_h(),
        choices,
        rule_h(),
        section(row![
            Space::new().width(Length::Fill),
            label(
                "← → choose · enter confirm",
                size::TEXT_SMALL,
                color::TEXT_TERTIARY
            ),
        ]),
    ])
    .width(size::DIALOG_W)
    .style(boxed)
    .into()
}

// ---------------------------------------------------------------- palette

/// The command palette: a hard-edged box over a scrim, the query on top,
/// then the matches with their chords right-aligned. The pick is gold; the
/// caret stays white so the pick keeps the only gold.
fn palette<'a>(app: &'a App, p: &'a Palette) -> Element<'a, Message> {
    let items = p.matches(&app.actions);
    let pick = p.pick.min(items.len().saturating_sub(1));
    let start = pick.saturating_sub(size::PALETTE_ROWS - 1);
    let input = container(row![
        label(": ", size::TEXT, color::TEXT_TERTIARY),
        line_edit(&p.line, color::TEXT, String::new()),
    ])
    .padding([size::CHROME_Y, size::PAD_X])
    .width(Length::Fill);
    let mut rows = Column::new();
    if items.is_empty() {
        rows = rows.push(
            container(label("no match", size::TEXT_SMALL, color::TEXT_TERTIARY))
                .padding([size::CHROME_Y, size::PAD_X]),
        );
    }
    for (i, item) in items
        .iter()
        .enumerate()
        .skip(start)
        .take(size::PALETTE_ROWS)
    {
        let on = i == pick;
        let (name, hint) = match item {
            Item::Action(a) => (a.name().to_owned(), chord_of(app, Target::Action(*a))),
            Item::Custom(c) => {
                let a = &app.actions[*c];
                (
                    a.name.clone(),
                    a.key.map(|k| k.to_string()).unwrap_or_else(|| {
                        Some(chord_of(app, Target::Custom(a.name.clone())))
                            .filter(|c| !c.is_empty())
                            .unwrap_or_else(|| "action".to_owned())
                    }),
                )
            }
        };
        let (bar, fill, fg) = if on {
            (
                Some(color::ACCENT),
                Some(color::ACCENT_FILL),
                color::ACCENT_TEXT,
            )
        } else {
            (None, None, color::TEXT_SECONDARY)
        };
        let r = container(row![
            block(size::BAR_W, Length::Fill, bar),
            container(
                row![
                    container(label(name, size::TEXT, fg))
                        .width(Length::Fill)
                        .clip(true),
                    label(hint, size::TEXT_SMALL, color::TEXT_TERTIARY),
                ]
                .align_y(alignment::Vertical::Center)
            )
            .padding([0.0, size::PAD_X - size::BAR_W])
            .height(Length::Fill)
            .align_y(alignment::Vertical::Center),
        ])
        .width(Length::Fill)
        .height(size::ROW_H)
        .style(move |_| container::Style {
            background: fill.map(Background::Color),
            ..Default::default()
        });
        rows = rows.push(mouse_area(r).on_press(Message::Run(*item)));
    }
    container(column![input, rule_h(), rows])
        .width(size::PALETTE_W)
        .style(boxed)
        .into()
}

/// The first chord bound to `t`, as fog.kdl spells it.
fn chord_of(app: &App, t: Target) -> String {
    app.keys
        .iter()
        .find(|(_, v)| **v == t)
        .map(|(k, _)| k.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{bytes, crumbs, group, job_subject};

    fn texts(v: Vec<(String, Option<Vec<u8>>)>) -> Vec<String> {
        v.into_iter().map(|c| c.0).collect()
    }

    #[test]
    fn crumbs_elide_from_the_left() {
        assert_eq!(texts(crumbs(b"/", 10)), ["/"]);
        assert_eq!(
            texts(crumbs(b"/home/u/src", 40)),
            ["/", "home/", "u/", "src"]
        );
        // 11 chars do not fit in 9: keep "…/u/src".
        assert_eq!(texts(crumbs(b"/home/u/src", 9)), ["…/", "u/", "src"]);
        assert_eq!(texts(crumbs(b"/home/u/src", 6)), ["…/", "src"]);
        // Not even "…/src": the name's own tail.
        assert_eq!(texts(crumbs(b"/home/u/src", 3)), ["…rc"]);
        assert_eq!(texts(crumbs(b"/home/u/src", 0)), ["…"]);
    }

    #[test]
    fn crumbs_open_their_folder() {
        let c = crumbs(b"/home/u/src", 40);
        let to: Vec<Option<&[u8]>> = c.iter().map(|c| c.1.as_deref()).collect();
        assert_eq!(
            to,
            [
                Some(&b"/"[..]),
                Some(&b"/home"[..]),
                Some(&b"/home/u"[..]),
                Some(&b"/home/u/src"[..])
            ]
        );
        assert_eq!(crumbs(b"/home/u/src", 9)[0].1, None);
    }

    #[test]
    fn bytes_are_binary() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(812), "812 B");
        assert_eq!(bytes(4096), "4.0 KiB");
        assert_eq!(bytes(12 * 1024 + 700), "12 KiB");
        assert_eq!(bytes(3 << 29), "1.5 GiB");
    }

    #[test]
    fn group_thousands() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(100_002), "100,002");
        assert_eq!(group(1_234_567), "1,234,567");
    }

    #[test]
    fn an_undo_row_names_what_it_undoes_never_slash() {
        use crate::ops::Tray;
        use fog_proto::{ConflictPolicy, JobSpec, JobStatus, Reply};
        let now = std::time::Instant::now();
        let state = |id, state| Reply::JobState { id, state };
        let mut t = Tray::default();
        t.undo();
        t.on_reply(&state(1, JobStatus::Queued), now);
        assert_eq!(job_subject(t.get(1).unwrap()), "undo last change");
        t.submit(JobSpec::Trash {
            paths: vec![b"/w/a.txt".to_vec()],
            on_conflict: ConflictPolicy::Fail,
        });
        t.on_reply(&Reply::JobAccepted { id: 2 }, now);
        t.on_reply(&state(2, JobStatus::Done), now);
        assert_eq!(job_subject(t.get(2).unwrap()), "a.txt");
        t.undo();
        t.on_reply(&state(3, JobStatus::Queued), now);
        assert_eq!(job_subject(t.get(3).unwrap()), "undo trash a.txt");
        // Another client's job with nothing known yet.
        t.on_reply(&state(4, JobStatus::Queued), now);
        assert_eq!(job_subject(t.get(4).unwrap()), "job");
    }
}
