// SPDX-License-Identifier: AGPL-3.0-only

//! Drawing (FOG §UI and navigation).
//!
//! Composition: the listing is the hero, a dense sortable table that takes
//! every spare pixel. Around it the chrome changes silhouette at every step
//! so nothing reads as a stack of panels: a rail of places on the left, then
//! top to bottom a strip of tabs (only with more than one), the breadcrumb
//! bar, a caption-height column header, the table, and a one-line status.
//!
//! One gold value, always: the cursor of whichever region has the keyboard
//! (the list's row, the sidebar's place, the palette's pick, or the path
//! editor's caret). Every other cursor and every mark goes neutral.

use fog_config::Target;
use fog_proto::{Entry, Kind, PlaceKind, SortKey};
use fog_widgets::virtual_list;
use iced::widget::text::Wrapping;
use iced::widget::{
    column, container, mouse_area, opaque, responsive, row, stack, text, Column, Row, Space,
};
use iced::{alignment, Background, Color, Element, Length, Size};
use jiff::tz::TimeZone;
use jiff::Timestamp;

use crate::app::{list_id, App, Focus, Fogd, Message, Overlay};
use crate::edit::LineEdit;
use crate::palette::{Item, Palette};
use crate::state::{Browser, Notice};
use crate::theme::{color, size};

pub fn view(app: &App) -> Element<'_, Message> {
    let b = app.tabs.active();
    let mut main = Column::new();
    if app.tabs.len() > 1 {
        main = main.push(tab_strip(app));
    }
    main = main
        .push(chrome(path_bar(app), Edge::Bottom))
        .push(
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
        )
        .push(chrome(status_line(app, b), Edge::Top));

    let mut body = Row::new();
    if app.sidebar {
        body = body.push(sidebar(app));
    }
    let base = container(body.push(main))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| ground(color::BASE, Some(color::TEXT)));

    match &app.overlay {
        Overlay::Palette(p) => stack![
            base,
            mouse_area(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(|_| ground(color::SCRIM, None))
            )
            .on_press(Message::Dismiss),
            container(opaque(palette(app, p)))
                .width(Length::Fill)
                .padding([size::PALETTE_TOP, 0.0])
                .align_x(alignment::Horizontal::Center),
        ]
        .into(),
        _ => base.into(),
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

/// Which cursor is the gold one.
fn list_focused(app: &App) -> bool {
    app.focus == Focus::List && app.overlay == Overlay::None
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

fn basename(path: &[u8]) -> String {
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
    let focused = app.focus == Focus::Places && app.overlay == Overlay::None;
    let sections: [(&str, &[PlaceKind]); 4] = [
        (
            "places",
            &[PlaceKind::Home, PlaceKind::UserDir, PlaceKind::Recent],
        ),
        ("bookmarks", &[PlaceKind::Bookmark]),
        ("devices", &[PlaceKind::Mount]),
        ("", &[PlaceKind::Trash]),
    ];
    let mut col = Column::new();
    for (title, kinds) in sections {
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
        _ => {
            let s = b.sort;
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
            row![
                mouse_area(
                    container(col("name", SortKey::Name))
                        .padding([0.0, size::PAD_X])
                        .width(Length::Fill)
                )
                .on_press(Message::Header(SortKey::Name)),
                right(col("size", SortKey::Size), size::SIZE_W, SortKey::Size),
                right(
                    col("modified", SortKey::Modified),
                    size::DATE_W,
                    SortKey::Modified
                ),
                right(col("type", SortKey::Type), size::TYPE_W, SortKey::Type),
                block(
                    size::PAD_X - size::BAR_W + if scrollbar { size::SCROLLBAR_W } else { 0.0 },
                    size::HAIRLINE,
                    None
                ),
            ]
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
    virtual_list(b.len(), size::ROW_H, move |i| {
        let cursor = match (i == b.selected, focused) {
            (false, _) => Cursor::No,
            (true, true) => Cursor::Focused,
            (true, false) => Cursor::Idle,
        };
        let r = list_row(b.row(i), cursor, b.is_marked(i), tz);
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

/// One row: `[bar] name[/] … size modified type`. Folders read primary,
/// files secondary; metadata still on its way from fogd reads `·`.
fn list_row(
    entry: Option<&Entry>,
    cursor: Cursor,
    marked: bool,
    tz: &TimeZone,
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
    let mut name = row![label(entry.display().into_owned(), size::TEXT, name_color)];
    if is_dir {
        name = name.push(label("/", size::TEXT, color::TEXT_TERTIARY));
    }
    let data = |s: String, w: f32| {
        label(s, size::TEXT_SMALL, color::TEXT_TERTIARY)
            .width(w)
            .align_x(alignment::Horizontal::Right)
    };
    let kind = entry.type_label();
    let body = row![
        container(name).width(Length::Fill).clip(true),
        data(size_text(entry), size::SIZE_W),
        data(date_text(entry.mtime_ns, tz), size::DATE_W),
        data(
            if kind.is_empty() {
                PENDING.to_owned()
            } else {
                kind.into_owned()
            },
            size::TYPE_W
        ),
    ]
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
        let total: u64 = sel.iter().filter_map(|e| e.size).sum();
        left = left.push(label(
            format!("{} selected · {}", group(sel.len()), bytes(total)),
            size::TEXT_SMALL,
            color::TEXT,
        ));
    }
    if b.pending.is_some() || !b.complete {
        left = left.push(label("listing…", size::TEXT_SMALL, color::TEXT_TERTIARY));
    }
    if let Some((path, errno)) = &b.error {
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
    right = right.push(label("0 jobs", size::TEXT_SMALL, color::TEXT_TERTIARY));
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
        .style(|_| container::Style {
            background: Some(Background::Color(color::CHROME)),
            border: iced::Border {
                color: color::RULE,
                width: size::HAIRLINE,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
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
    use super::{bytes, crumbs, group};

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
}
