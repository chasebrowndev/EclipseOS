// SPDX-License-Identifier: AGPL-3.0-only

//! Drawing (FOG §UI and navigation, §Visual design).
//!
//! Composition: the listing is the hero, a dense sortable table laid bare on
//! the window's tinted glass and given every spare pixel. Around it the
//! chrome changes silhouette at every step so nothing reads as a stack of
//! panels: a flush, darker places column on the left; above the table a
//! strip of bare tabs (only with more than one) and the path as an inset
//! field of mono crumbs; then quiet column labels over a hairline, the
//! table, the job tray as a glass card of meters (only while there are
//! jobs) and one line of bare status text. The palette, the name sheet and
//! the dialogs are glass sheets over a light scrim, blurring a snapshot of
//! the window. No gradients anywhere (STYLE.md).
//!
//! One gold value, always: the cursor of whichever region has the keyboard
//! (the list's pill, the sidebar's place, the tray's job, the palette's or a
//! dialog's pick, or the caret of the path or name being typed). The active
//! tab's marker is the only other gold, and it is small. Every other
//! cursor, every mark and every progress bar goes neutral.

use std::time::{Duration, Instant};

use fog_config::{Action, Target};
use fog_proto::{Entry, JobStatus, Kind, SortKey, StatReply, TrashItem};
use fog_widgets::{draggable, elide, glass, virtual_list};
use iced::widget::text::Wrapping;
use iced::widget::{
    column, container, mouse_area, opaque, responsive, row, stack, text, Column, Row, Space,
};
use iced::{alignment, Background, Border, Color, Element, Length, Padding, Shadow, Size};
use jiff::tz::TimeZone;
use jiff::Timestamp;

use crate::app::{list_id, App, DropAt, Focus, Fogd, JobCtl, Message, Overlay, SECTIONS};
use crate::clip::ClipOp;
use crate::edit::LineEdit;
use crate::ops::{resolution_label, Confirm, Conflict, Job, JobKind, NameEntry, NameFor};
use crate::palette::{Item, Palette};
use crate::parts::{self, Glyph};
use crate::state::{join, split_parent, Browser, Notice};
use crate::theme::{alpha, color, font, look, motion, size};

pub fn view(app: &App) -> Element<'_, Message> {
    let l = look();
    let b = app.tabs.active();
    let m = &app.motion;
    let mut main = Column::new();
    if app.tabs.len() > 1 {
        main = main.push(tab_strip(app));
    }
    main = main.push(capsule(app)).push(
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
    // While it folds away the tray shows what it last held, and its room
    // goes with the spring: the list never jumps under the pointer.
    if !app.tray.jobs.is_empty() {
        main = main.push(tray(app, &app.tray.jobs, m.tray.value));
    } else if m.tray.value > 0.0 && !app.tray.gone.is_empty() {
        main = main.push(tray(app, &app.tray.gone, m.tray.value));
    }
    main = main.push(
        container(status_line(app, b))
            .padding([size::CHROME_Y, size::PAD_X + size::PILL_X])
            .width(Length::Fill),
    );

    let mut body = Row::new();
    // Kept while it slides away, gone once it has.
    if m.sidebar.value > REST {
        body = body.push(sidebar(app, m.sidebar.value));
    }
    let base = container(body.push(main))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| ground(l.window, Some(color::TEXT)));

    let k = m.sheet.value;
    // A waiting conflict outranks any overlay: its job is stalled on it.
    if let Some(c) = app.conflicts.first() {
        return modal(
            app,
            base,
            conflict_dialog(app, c, ink(k)),
            None,
            Place::Centre,
        );
    }
    match &app.overlay {
        Overlay::Palette(p) => modal(
            app,
            base,
            palette(app, p, ink(k)),
            Some(Message::Dismiss),
            Place::Top,
        ),
        Overlay::Confirm(c) => modal(
            app,
            base,
            confirm_dialog(c, ink(k)),
            Some(Message::Confirm(false)),
            Place::Centre,
        ),
        Overlay::Name(
            n @ NameEntry {
                what: NameFor::Folder | NameFor::File,
                ..
            },
        ) => modal(app, base, name_sheet(n, ink(k)), None, Place::Top),
        _ => base.into(),
    }
}

/// A spring this close to zero draws nothing.
const REST: f32 = 0.01;

/// Content opacity for a sheet or panel at spring value `k`: it trails the
/// glass a little, so text never floats without its ground.
fn ink(k: f32) -> f32 {
    ((k - 0.2) / 0.8).clamp(0.0, 1.0)
}

/// `c` faded by `k`.
fn fade(c: Color, k: f32) -> Color {
    alpha(c, c.a * k)
}

#[derive(Clone, Copy)]
enum Place {
    /// Under the top edge, like the palette.
    Top,
    Centre,
}

/// A sheet floating over the window: a light scrim (a click on it sends
/// `dismiss`), then the glass, blurring the window's snapshot, scaling and
/// fading in on its spring.
fn modal<'a>(
    app: &'a App,
    base: impl Into<Element<'a, Message>>,
    sheet: Element<'a, Message>,
    dismiss: Option<Message>,
    at: Place,
) -> Element<'a, Message> {
    let l = look();
    let m = &app.motion;
    let k = m.sheet.value;
    let scale = motion::SHEET_FROM + (1.0 - motion::SHEET_FROM) * k;
    let panel = glass(opaque(sheet), l.sheet())
        .backdrop(m.backdrop.clone())
        .opacity(k)
        .scale(scale);
    let placed = container(panel).width(Length::Fill).height(Length::Fill);
    let placed = match at {
        Place::Top => placed
            .padding(Padding::ZERO.top(size::PALETTE_TOP))
            .align_x(alignment::Horizontal::Center),
        Place::Centre => placed.center(Length::Fill),
    };
    stack![base.into(), scrim(dismiss, k.min(1.0)), placed].into()
}

/// The dimmed window under a sheet; a click on it sends `dismiss`. Opaque:
/// the wheel does not scroll the list under a sheet (its snapshot would
/// no longer match).
fn scrim<'a>(dismiss: Option<Message>, k: f32) -> Element<'a, Message> {
    let s = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| ground(fade(color::SCRIM, k), None)),
    );
    match dismiss {
        Some(m) => opaque(s.on_press(m)),
        None => opaque(s),
    }
}

fn ground(bg: Color, fg: Option<Color>) -> container::Style {
    container::Style {
        background: Some(Background::Color(bg)),
        text_color: fg,
        ..Default::default()
    }
}

/// A rounded wash: pills, the tray's meters, an inset field.
fn pill(fill: Color, border: Color, glow: Option<Shadow>, radius: f32) -> container::Style {
    container::Style {
        background: Some(Background::Color(fill)),
        border: Border {
            color: border,
            width: if border.a > 0.0 { size::HAIRLINE } else { 0.0 },
            radius: radius.into(),
        },
        shadow: glow.unwrap_or_default(),
        ..Default::default()
    }
}

/// Data text: JetBrains Mono (STYLE.md: "mono for data, units, paths,
/// timestamps").
fn label<'a>(s: impl text::IntoFragment<'a>, sz: f32, c: Color) -> text::Text<'a> {
    text(s)
        .size(sz)
        .color(c)
        .font(font::DATA)
        .wrapping(Wrapping::None)
}

/// Interface text: Instrument Sans.
fn caption<'a>(s: impl text::IntoFragment<'a>, sz: f32, c: Color) -> text::Text<'a> {
    label(s, sz, c).font(font::UI)
}

/// A filled block: rules, the caret, a meter.
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

fn rule_h<'a>(k: f32) -> Element<'a, Message> {
    block(Length::Fill, size::HAIRLINE, Some(fade(color::RULE, k)))
}

/// Nothing modal is open: the regions' own cursors may take the gold.
fn unobstructed(app: &App) -> bool {
    matches!(app.overlay, Overlay::None) && app.conflicts.is_empty()
}

/// Which cursor is the gold one.
fn list_focused(app: &App) -> bool {
    app.focus == Focus::List && unobstructed(app)
}

/// How a cursor, mark or pick is drawn: a flat gold wash while its region
/// has the keyboard, else a neutral one. No edge, no glow (STYLE.md: "no
/// gradients of the accent", accent tints are flat fills).
fn cursor_style(focused: bool) -> container::Style {
    let fill = if focused {
        color::ACCENT_FILL
    } else {
        color::MARK_FILL
    };
    pill(fill, Color::TRANSPARENT, None, look().chip_radius)
}

// ---------------------------------------------------------------- tabs

/// Tabs, only when there are two or more: bare names, the active one white
/// over a short gold marker.
fn tab_strip(app: &App) -> Element<'_, Message> {
    let l = look();
    let mut strip = Row::new()
        .height(size::TAB_H)
        .spacing(size::PILL_X)
        .align_y(alignment::Vertical::Bottom);
    for (i, b) in app.tabs.iter().enumerate() {
        let on = i == app.tabs.index();
        let name = basename(b.target());
        let marker = container(Space::new())
            .width(size::TAB_MARK_W)
            .height(size::TAB_MARK_H)
            .style(move |_| {
                pill(
                    if on {
                        color::ACCENT
                    } else {
                        Color::TRANSPARENT
                    },
                    Color::TRANSPARENT,
                    None,
                    l.chip_radius,
                )
            });
        let tab = column![
            container(caption(
                name,
                size::TEXT_SMALL,
                if on {
                    color::TEXT
                } else {
                    color::TEXT_TERTIARY
                },
            ))
            .max_width(size::TAB_MAX_W)
            .clip(true),
            marker,
        ]
        .spacing(size::PILL_X / 2.0)
        .align_x(alignment::Horizontal::Center)
        .padding([0.0, size::PAD_X / 2.0]);
        strip = strip.push(mouse_area(tab).on_press(Message::Tab(i)));
    }
    container(strip)
        .padding(
            Padding::ZERO
                .left(size::INSET + size::PILL_X)
                .top(size::INSET / 2.0),
        )
        .width(Length::Fill)
        .into()
}

/// A tray row's subject: the first item the job named (its top-level
/// source, never a file inside it) and how many more; for another client's
/// job, whose spec we never saw, the path being worked on. An undo says
/// what it undoes, as far as the tray knows. A new folder or file, once
/// made, says so: "created NewDir".
fn job_subject(j: &Job) -> String {
    if j.subject.is_empty() && j.kind == JobKind::Other && !j.current.is_empty() {
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
        (JobKind::Mkdir | JobKind::CreateFile, _) if j.status == JobStatus::Done => {
            format!("created {s}")
        }
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

/// The places column: flush with the window's left edge on a darker shade
/// (STYLE.md "Left sidebar: rgba(0,0,0,.4) + blur, width 214"), a hairline
/// on its right, sections under small mono uppercase labels. The current
/// folder's place is a neutral wash; the sidebar's own cursor takes the
/// gold wash and bar only while it has the keyboard. `k` is its spring:
/// it opens by width alone and never fades, so its edge is always where
/// the list begins.
fn sidebar(app: &App, k: f32) -> Element<'_, Message> {
    let here = app.tabs.active().target();
    let focused = app.focus == Focus::Places && unobstructed(app);
    let drop = app.drop_at();
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
                container(label(
                    title.to_uppercase(),
                    size::MICRO,
                    color::TEXT_TERTIARY,
                ))
                .height(size::HEADER_H)
                .align_y(alignment::Vertical::Center)
                .padding([0.0, size::PAD_X]),
            );
        }
        for i in members {
            let p = &app.places[i];
            let state = PlaceState {
                cursor: focused && i == app.place_sel,
                current: p.path == here,
                hover: app.place_hover == Some(i) && app.drag.is_none(),
                hot: drop == Some(DropAt::Place(i)),
            };
            col = col.push(
                mouse_area(place_row(&p.label, state))
                    .on_press(Message::Go(p.path.clone()))
                    .on_enter(Message::PlaceHover(i))
                    .on_exit(Message::PlaceUnhover(i)),
            );
        }
    }
    if app.places.is_empty() {
        col = col.push(block(Length::Fill, size::SECTION_GAP, None)).push(
            container(caption("no places", size::TEXT_SMALL, color::TEXT_TERTIARY))
                .padding([0.0, size::PAD_X]),
        );
    }
    let width = (size::SIDEBAR_W * k).max(0.0);
    let column = container(
        container(col)
            .width(size::SIDEBAR_W - size::HAIRLINE)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .style(move |_| ground(color::SIDEBAR, None));
    row![
        column,
        block(size::HAIRLINE, Length::Fill, Some(color::RULE))
    ]
    .width(width)
    .height(Length::Fill)
    .clip(true)
    .into()
}

#[derive(Clone, Copy)]
struct PlaceState {
    /// The sidebar's cursor, with the keyboard here: the gold one.
    cursor: bool,
    /// The folder shown is this place.
    current: bool,
    hover: bool,
    /// A drag over it would land here.
    hot: bool,
}

/// One place: its stand-in icon and name. The focused cursor is a gold
/// wash with a 3px gold bar at its left (STYLE.md: "active item gets a
/// translucent fill + a 3px yellow left bar"); the current place, a
/// neutral wash; under the pointer, the hover wash; under a drag, the drop
/// well.
fn place_row(name: &str, st: PlaceState) -> Element<'static, Message> {
    let fg = if st.cursor || st.current {
        color::TEXT
    } else {
        color::TEXT_SECONDARY
    };
    let r = look().chip_radius;
    let style = move |_: &iced::Theme| {
        if st.hot {
            return parts::drop_well();
        }
        match (st.cursor, st.current, st.hover) {
            (true, _, _) => cursor_style(true),
            (false, true, _) => cursor_style(false),
            (false, false, true) => pill(color::HOVER, Color::TRANSPARENT, None, r),
            _ => container::Style::default(),
        }
    };
    let bar = block(
        size::NAV_BAR_W,
        Length::Fill,
        st.cursor.then_some(color::ACCENT),
    );
    let body = row![
        parts::glyph(Glyph::Place),
        elide(name.to_owned())
            .size(size::TEXT)
            .font(font::UI)
            .color(fg),
    ]
    .spacing(size::GLYPH_GAP)
    .align_y(alignment::Vertical::Center);
    container(
        container(row![
            bar,
            container(body)
                .padding([0.0, size::PAD_X - size::PILL_X - size::NAV_BAR_W])
                .height(Length::Fill)
                .align_y(alignment::Vertical::Center),
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .style(style),
    )
    .padding([size::HAIRLINE, size::PILL_X])
    .width(Length::Fill)
    .height(size::PLACE_H)
    .into()
}

// ---------------------------------------------------------------- path bar

/// The path as an inset field over the table: one step off the window's
/// tint with a hairline edge (STYLE.md: inset radius 9-11, borders
/// rgba(255,255,255,.10)), not a panel of its own.
fn capsule(app: &App) -> Element<'_, Message> {
    let r = look().inset_radius;
    container(
        container(path_bar(app))
            .padding([0.0, size::PILL_X])
            .height(size::CAPSULE_H)
            .width(Length::Fill)
            .align_y(alignment::Vertical::Center)
            .style(move |_| pill(color::FIELD, color::BORDER, None, r)),
    )
    .padding(size::INSET)
    .width(Length::Fill)
    .into()
}

/// Breadcrumbs, or the path editor while it is open.
fn path_bar(app: &App) -> Element<'_, Message> {
    if let Overlay::Path(p) = &app.overlay {
        let ghost = p.ghost(app.tabs.active().show_hidden).unwrap_or_default();
        return container(line_edit(&p.line, color::ACCENT, ghost, 1.0))
            .padding([0.0, size::CRUMB_X])
            .into();
    }
    let path = app.tabs.active().target().to_vec();
    let crumbs = crumb_bar(path);
    if !app.in_trash() {
        return crumbs;
    }
    // In the trash the bar carries what can be done there.
    let act = |name: &'static str, a: Action| {
        parts::hover(
            row![
                caption(name, size::TEXT_SMALL, color::TEXT_SECONDARY),
                label(
                    format!(" {}", chord_of(app, Target::Action(a))),
                    size::MONO,
                    color::TEXT_TERTIARY
                ),
            ]
            .align_y(alignment::Vertical::Center),
            Message::Act(a),
            [size::HAIRLINE * 2.0, size::CRUMB_X * 2.0],
        )
    };
    row![
        container(crumbs).width(Length::Fill),
        row![
            act("restore", Action::Restore),
            act("delete permanently", Action::Delete)
        ]
        .spacing(size::PILL_X),
    ]
    .spacing(size::GAP)
    .align_y(alignment::Vertical::Center)
    .into()
}

/// The path's crumbs, in mono: ancestors secondary, each a hover target
/// that opens it; the current folder in full white.
fn crumb_bar<'a>(path: Vec<u8>) -> Element<'a, Message> {
    responsive(move |room: Size| {
        // The path font is monospace, so a width is a character count,
        // less each crumb's own padding.
        let per = |w: f32| (w / (size::MONO * size::MONO_ADVANCE)) as usize;
        let mut segs = crumbs(&path, per(room.width));
        let pads = segs.len() as f32 * size::CRUMB_X * 2.0;
        if pads > 0.0 {
            segs = crumbs(&path, per(room.width - pads));
        }
        let last = segs.len().saturating_sub(1);
        let mut r = Row::new().align_y(alignment::Vertical::Center);
        for (i, (s, to)) in segs.into_iter().enumerate() {
            let fg = if i == last {
                color::TEXT
            } else {
                color::TEXT_SECONDARY
            };
            // An ancestor's trailing `/` is drawn apart, quieter, outside
            // its hover wash: the wash holds the name alone.
            let (name, sep) = match s.strip_suffix('/') {
                Some(n) if !n.is_empty() => (n.to_owned(), true),
                _ => (s, false),
            };
            let l = label(name, size::MONO, fg);
            let pad = [size::HAIRLINE * 2.0, size::CRUMB_X];
            r = match to {
                Some(to) if i != last => r.push(parts::hover(l, Message::Go(to), pad)),
                _ => r.push(container(l).padding(pad)),
            };
            if sep {
                r = r.push(label("/", size::MONO, color::TEXT_TERTIARY));
            }
        }
        container(r).width(Length::Fill).clip(true).into()
    })
    .height(Length::Shrink)
    .into()
}

/// One editable line: text, a caret in `caret`'s colour, then `ghost` (the
/// completion Tab would take) in tertiary; all faded by `k`.
fn line_edit<'a>(line: &LineEdit, caret: Color, ghost: String, k: f32) -> Element<'a, Message> {
    let (before, after) = line.halves();
    row![
        label(before.to_owned(), size::TEXT, fade(color::TEXT, k)),
        block(size::CARET_W, size::TEXT, Some(fade(caret, k))),
        label(after.to_owned(), size::TEXT, fade(color::TEXT, k)),
        label(ghost, size::TEXT, fade(color::TEXT_TERTIARY, k)),
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
    let lead = size::PILL_X + size::PAD_X;
    let content: Element<'_, Message> = match &app.overlay {
        Overlay::Path(p) => {
            let cands = p.candidates(b.show_hidden);
            let mut r = Row::new().spacing(size::GAP);
            if cands.is_empty() {
                r = r.push(caption(
                    "no folders",
                    size::TEXT_SMALL,
                    color::TEXT_TERTIARY,
                ));
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
                .padding([0.0, lead])
                .width(Length::Fill)
                .clip(true)
                .into()
        }
        _ => {
            let s = b.sort;
            let trash = app.in_trash();
            // A column label: quiet, the sorted one a step up, each a hover
            // target that sorts by it (in fogd).
            let col = |name: &'static str, key: SortKey, w: Length, at| {
                let on = s.key == key;
                let arrow = match (on, s.reverse) {
                    (false, _) => "",
                    (true, false) => " ↑",
                    (true, true) => " ↓",
                };
                parts::hover(
                    caption(
                        format!("{name}{arrow}"),
                        size::TEXT_SMALL,
                        if on {
                            color::TEXT_SECONDARY
                        } else {
                            color::TEXT_TERTIARY
                        },
                    )
                    .width(Length::Fill)
                    .align_x(at),
                    Message::Header(key),
                    [size::HAIRLINE * 2.0, size::COL_PAD],
                )
                .width(w)
            };
            let left = alignment::Horizontal::Left;
            let right = alignment::Horizontal::Right;
            let mut r = row![col("name", SortKey::Name, Length::Fill, left)];
            if trash {
                r = r.push(
                    container(caption("from", size::TEXT_SMALL, color::TEXT_TERTIARY))
                        .width(size::FROM_W),
                );
            }
            r = r
                .push(col("size", SortKey::Size, size::SIZE_W.into(), right))
                // fogd sorts the trash by the files' own times: the label
                // says what the column shows.
                .push(col(
                    if trash { "deleted" } else { "modified" },
                    SortKey::Modified,
                    size::DATE_W.into(),
                    right,
                ));
            if !trash {
                r = r.push(col("kind", SortKey::Type, size::TYPE_W.into(), left));
            }
            container(r.align_y(alignment::Vertical::Center))
                .padding(
                    Padding::from([0.0, size::PILL_X])
                        .right(size::PILL_X + if scrollbar { size::SCROLLBAR_W } else { 0.0 }),
                )
                .into()
        }
    };
    column![
        container(content)
            .height(size::HEADER_H)
            .width(Length::Fill)
            .align_y(alignment::Vertical::Center),
        container(rule_h(1.0)).padding([0.0, lead]),
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
        return container(caption(msg, size::TEXT_SMALL, color::TEXT_TERTIARY))
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
    let m = &app.motion;
    let hover = m.hovered.map(|i| (i, m.hover.value));
    let dragging = app.drag.is_some();
    let hot = match app.drop_at() {
        Some(DropAt::Row(i)) => Some(i),
        _ => None,
    };
    let ghost = app.drag.as_ref().map(|d| {
        (
            if d.dir { Glyph::Folder } else { Glyph::File },
            d.name.clone(),
        )
    });
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
            // Under a drag the pointer aims; only the target lights.
            hover: hover
                .filter(|(h, _)| *h == i && !dragging)
                .map_or(0.0, |(_, k)| k),
            hot: hot == Some(i),
        };
        let r = list_row(entry, cursor, b.is_marked(i), tz, extra);
        // The whole row is the handle: a click picks it, a double click
        // opens it, a drag carries it (with the selection it is in).
        let mut d = draggable(r)
            .on_click(Message::Row(i))
            .on_double_click(Message::OpenRow(i))
            .on_drag(Message::Lift(i))
            .on_release(Message::Release)
            .lifted(dragging);
        if let Some((g, name)) = &ghost {
            d = d.ghost(parts::ghost(*g, name.clone()), size::GHOST_GAP);
        }
        mouse_area(d)
            .on_enter(Message::Hover(i))
            .on_exit(Message::Unhover(i))
            .into()
    })
    // The cursor's pill slides under the rows on its spring.
    .underlay(m.pill.value, move || {
        container(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(move |_| cursor_style(focused)),
        )
        .padding([size::HAIRLINE, size::PILL_X])
        .width(Length::Fill)
        .height(Length::Fill)
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
    /// The pointer's wash, 0 to 1.
    hover: f32,
    /// A drag over this folder would land in it.
    hot: bool,
}

/// One row: `[icon] name … size modified kind`. The name is the reading
/// line, in the interface face, elided to its column; the metadata is mono
/// and a step quieter, and still-coming metadata reads `·`. In the trash a
/// row is named by where it came from: `name from size deleted`. The
/// cursor's pill is not the row's: it slides beneath (see [`list`]).
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
    let fill = match (cursor, marked) {
        _ if extra.hot => None,
        (Cursor::No, true) => Some(color::MARK_FILL),
        (Cursor::No, false) if extra.hover > 0.0 => Some(fade(color::HOVER, extra.hover)),
        _ => None,
    };
    let item = extra.trash.flatten();
    let shown = match item {
        Some(t) => basename(&t.original_path),
        None => entry.display().into_owned(),
    };
    let glyph = parts::glyph(if is_dir { Glyph::Folder } else { Glyph::File });
    let name: Element<'static, Message> = match extra.edit {
        // The caret is the gold while a name is typed; the row idles.
        Some(line) => line_edit(line, color::ACCENT, String::new(), 1.0),
        None => elide(shown)
            .size(size::TEXT)
            .font(if cursor == Cursor::Focused {
                font::UI_MEDIUM
            } else {
                font::UI
            })
            .color(if cursor == Cursor::Focused {
                color::ACCENT_TEXT
            } else {
                color::TEXT
            })
            .into(),
    };
    let cell = |e: Element<'static, Message>, w: Length| {
        container(e)
            .padding([0.0, size::COL_PAD])
            .width(w)
            .clip(true)
    };
    let data = |s: String, w: f32| {
        cell(
            label(s, size::MONO, color::TEXT_SECONDARY)
                .width(Length::Fill)
                .align_x(alignment::Horizontal::Right)
                .into(),
            w.into(),
        )
    };
    let mut body = row![cell(
        row![glyph, name]
            .spacing(size::GLYPH_GAP)
            .align_y(alignment::Vertical::Center)
            .into(),
        Length::Fill
    )];
    match extra.trash {
        Some(item) => {
            let from = item
                .and_then(|t| split_parent(&t.original_path))
                // Elided from the left, like the path bar: the tail is what
                // tells two trashed items' folders apart.
                .map(|(dir, _)| {
                    let room = size::FROM_W - 2.0 * size::COL_PAD;
                    let fits = (room / (size::MONO * size::MONO_ADVANCE)) as usize;
                    crumbs(&dir, fits).into_iter().map(|(t, _)| t).collect()
                })
                .unwrap_or_else(|| PENDING.to_owned());
            let deleted = item
                .and_then(|t| t.deleted_s)
                .map(|s| i128::from(s) * 1_000_000_000);
            body = body
                .push(cell(
                    label(from, size::MONO, color::TEXT_TERTIARY).into(),
                    size::FROM_W.into(),
                ))
                .push(data(size_text(entry), size::SIZE_W))
                .push(data(date_text(deleted, tz), size::DATE_W));
        }
        None => {
            body = body
                .push(data(size_text(entry), size::SIZE_W))
                .push(data(date_text(entry.mtime_ns, tz), size::DATE_W))
                .push(cell(
                    caption(kind_text(entry), size::TEXT_SMALL, color::TEXT_TERTIARY).into(),
                    size::TYPE_W.into(),
                ));
        }
    }
    let body = body
        .align_y(alignment::Vertical::Center)
        .height(Length::Fill);
    let r = look().chip_radius;
    let hot = extra.hot;
    container(
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| match (hot, fill) {
                (true, _) => parts::drop_well(),
                (false, Some(f)) => pill(f, Color::TRANSPARENT, None, r),
                (false, None) => container::Style::default(),
            }),
    )
    .padding([size::HAIRLINE, size::PILL_X])
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The kind column, in words: `Folder`, `Link`, an extension in capitals
/// (`PDF`), `Program`, `Document`. The sort key stays fogd's `type_label`;
/// this only names it.
pub fn kind_text(e: &Entry) -> String {
    match e.kind {
        Kind::Dir => return "Folder".to_owned(),
        Kind::Symlink => return "Link".to_owned(),
        Kind::Unknown => return PENDING.to_owned(),
        Kind::File | Kind::Other => {}
    }
    match e.type_label().as_ref() {
        "file" => "Document".to_owned(),
        "exec" => "Program".to_owned(),
        "fifo" => "Pipe".to_owned(),
        "socket" => "Socket".to_owned(),
        "char" | "block" => "Device".to_owned(),
        "other" => "Other".to_owned(),
        ext => ext.to_uppercase(),
    }
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
    left = left.push(caption(count, size::TEXT_SMALL, color::TEXT_SECONDARY));
    let hidden = b.total() - b.unfiltered();
    if hidden > 0 {
        left = left.push(caption(
            format!("{} hidden", group(hidden)),
            size::TEXT_SMALL,
            color::TEXT_TERTIARY,
        ));
    }
    if !b.filter.is_empty() {
        left = left.push(row![
            caption("filter ", size::TEXT_SMALL, color::TEXT_TERTIARY),
            caption(b.filter.clone(), size::TEXT_SMALL, color::TEXT),
        ]);
    }
    let sel = b.selection();
    if !sel.is_empty() {
        left = left.push(caption(
            match b.selected_bytes() {
                Some(n) => format!("{} selected · {}", group(sel.len()), bytes(n)),
                None => format!("{} selected", group(sel.len())),
            },
            size::TEXT_SMALL,
            color::TEXT,
        ));
    }
    if b.pending.is_some() || !b.complete {
        left = left.push(caption("listing…", size::TEXT_SMALL, color::TEXT_TERTIARY));
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
        left = left.push(caption(s, size::TEXT_SMALL, c));
    } else if let Some((path, errno)) = &b.error {
        left = left.push(caption(
            format!("{} — {}", strerror(*errno), String::from_utf8_lossy(path)),
            size::TEXT_SMALL,
            color::DANGER,
        ));
    } else if let Some(n) = &b.notice {
        let (s, c) = notice(n);
        left = left.push(caption(s, size::TEXT_SMALL, c));
    }
    let mut right = Row::new().spacing(size::GAP);
    if let Some((free, _total)) = b.space {
        right = right.push(caption(
            format!("{} free", bytes(free)),
            size::TEXT_SMALL,
            color::TEXT_TERTIARY,
        ));
    }
    // Jobs and the daemon speak only when there is something to say.
    let live = app.tray.live();
    if live > 0 {
        right = right.push(caption(
            match live {
                1 => "1 job".to_owned(),
                n => format!("{} jobs", group(n)),
            },
            size::TEXT_SMALL,
            color::TEXT_SECONDARY,
        ));
    }
    match app.fogd {
        Fogd::Up => {}
        Fogd::Connecting => {
            right = right.push(caption(
                "connecting…",
                size::TEXT_SMALL,
                color::TEXT_TERTIARY,
            ));
        }
        Fogd::Down => {
            right = right.push(caption("fogd not running", size::TEXT_SMALL, color::DANGER));
        }
    }
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
        Notice::Skipped(k) => (
            format!("{} skipped · nothing changed", k.label()),
            color::TEXT_SECONDARY,
        ),
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

/// The job tray: a glass card of meters under the table, one per job,
/// each `kind  file  amount  eta  controls` over a rounded progress bar.
/// The cursor is gold only while the tray has the keyboard. `k` is its
/// spring: it fades and settles in when the first job arrives, and its
/// room opens and closes with it.
fn tray<'a>(app: &'a App, jobs: &'a [Job], k: f32) -> Element<'a, Message> {
    let l = look();
    let focused = app.focus == Focus::Jobs && unobstructed(app);
    let now = Instant::now();
    let ink = ink(k);
    let start = app.tray.cursor.saturating_sub(size::TRAY_ROWS - 1);
    let mut col = Column::new().spacing(size::HAIRLINE * 2.0);
    for (i, j) in jobs.iter().enumerate().skip(start).take(size::TRAY_ROWS) {
        col = col.push(job_row(j, focused && i == app.tray.cursor, now, ink));
    }
    let hidden = jobs.len().saturating_sub(size::TRAY_ROWS);
    if hidden > 0 {
        let hint = chord_of(app, Target::Action(Action::FocusJobs));
        col = col.push(
            container(caption(
                format!("{} more · {hint}", group(hidden)),
                size::TEXT_SMALL,
                fade(color::TEXT_TERTIARY, ink),
            ))
            .padding([0.0, size::PAD_X])
            .height(size::HEADER_H)
            .align_y(alignment::Vertical::Center),
        );
    }
    let card = glass(
        container(col).padding(size::PANEL_PAD).width(Length::Fill),
        l.panel(),
    )
    .opacity(k)
    .scale(motion::SHEET_FROM + (1.0 - motion::SHEET_FROM) * k);
    let shown = jobs.len().min(size::TRAY_ROWS) as f32;
    let full = size::INSET
        + 2.0 * size::PANEL_PAD
        + shown * (size::ROW_H + size::PROGRESS_H + 4.0 * size::HAIRLINE)
        + (shown - 1.0) * 2.0 * size::HAIRLINE
        + if hidden > 0 {
            size::HEADER_H + 2.0 * size::HAIRLINE
        } else {
            0.0
        };
    container(card)
        .padding(Padding::from(size::INSET).bottom(0.0))
        .width(Length::Fill)
        .height(full * k.clamp(0.0, 1.0))
        .clip(true)
        .into()
}

fn job_row(j: &Job, cursor: bool, now: Instant, k: f32) -> Element<'static, Message> {
    let kind_fg = if cursor {
        color::ACCENT_TEXT
    } else {
        color::TEXT_SECONDARY
    };
    let failed = matches!(j.status, JobStatus::Failed { .. });
    let mut what = job_subject(j);
    if let JobStatus::Failed { msg, .. } = &j.status {
        what = format!("{what} — {msg}");
    }
    let amount = job_amount(j);
    let (state, state_fg) = match &j.status {
        JobStatus::Queued => ("queued".to_owned(), color::TEXT_TERTIARY),
        JobStatus::Running => (
            j.eta(now).map(clock).unwrap_or_default(),
            color::TEXT_TERTIARY,
        ),
        JobStatus::Paused => ("paused".to_owned(), color::TEXT),
        JobStatus::Conflict { .. } => ("waiting".to_owned(), color::TEXT),
        JobStatus::Done if j.skipped_all() => ("skipped".to_owned(), color::TEXT_SECONDARY),
        JobStatus::Done => ("done".to_owned(), color::TEXT_TERTIARY),
        JobStatus::Failed { .. } => ("failed".to_owned(), color::DANGER),
        JobStatus::Cancelled => ("cancelled".to_owned(), color::TEXT_TERTIARY),
    };
    let ctl = |key: &'static str, word: &'static str, c: JobCtl| {
        parts::hover(
            row![
                label(key, size::MONO, fade(color::TEXT_TERTIARY, k)),
                caption(
                    format!(" {word}"),
                    size::TEXT_SMALL,
                    fade(color::TEXT_SECONDARY, k)
                ),
            ]
            .align_y(alignment::Vertical::Center),
            Message::Job(j.id, c),
            [size::HAIRLINE * 2.0, size::CRUMB_X * 2.0],
        )
    };
    let mut controls = Row::new().spacing(size::PILL_X);
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
        label(s, size::TEXT_SMALL, fade(c, k))
            .width(w)
            .align_x(alignment::Horizontal::Right)
    };
    let body = row![
        caption(j.kind.label(), size::TEXT_SMALL, fade(kind_fg, k)).width(size::KIND_W),
        elide(what)
            .size(size::TEXT)
            .font(font::UI)
            .color(fade(if failed { color::DANGER } else { color::TEXT }, k)),
        right(amount, size::AMOUNT_W, color::TEXT_SECONDARY),
        right(state, size::ETA_W, state_fg),
        container(controls).padding(Padding::ZERO.left(size::GAP)),
    ]
    .align_y(alignment::Vertical::Center)
    .height(size::ROW_H);
    container(column![body, progress(j.fraction(), failed, k)])
        .padding([
            size::HAIRLINE * 2.0,
            size::PAD_X - size::PANEL_PAD + size::PILL_X,
        ])
        .width(Length::Fill)
        .style(move |_| {
            if cursor {
                cursor_style(true)
            } else {
                container::Style::default()
            }
        })
        .into()
}

/// A tray row's amount: bytes, files or what was skipped. A new folder or
/// file is one thing: its subject says it all ("1 / 1 files" said nothing).
fn job_amount(j: &Job) -> String {
    if matches!(j.kind, JobKind::Mkdir | JobKind::CreateFile) {
        return String::new();
    }
    if j.files_skipped > 0 && !j.live() {
        // Bytes read as copied; say what was left alone instead.
        format!(
            "{} / {} skipped",
            group(j.files_skipped as usize),
            group(j.files_total as usize)
        )
    } else if j.bytes_total > 0 {
        format!("{} / {}", bytes(j.bytes_done), bytes(j.bytes_total))
    } else if j.files_total > 0 {
        format!(
            "{} / {} files",
            group(j.files_done as usize),
            group(j.files_total as usize)
        )
    } else {
        String::new()
    }
}

/// A rounded meter: the done part over the track.
fn progress<'a>(fraction: Option<f32>, failed: bool, k: f32) -> Element<'a, Message> {
    const STEPS: f32 = 1000.0;
    let done = (fraction.unwrap_or(0.0).clamp(0.0, 1.0) * STEPS) as u16;
    let tone = fade(
        if failed {
            color::DANGER
        } else {
            color::PROGRESS
        },
        k,
    );
    let r = size::PROGRESS_H / 2.0;
    let mut bar = Row::new().height(size::PROGRESS_H);
    if done > 0 {
        bar = bar.push(
            container(Space::new())
                .width(Length::FillPortion(done))
                .height(Length::Fill)
                .style(move |_| pill(tone, Color::TRANSPARENT, None, r)),
        );
    }
    if done < STEPS as u16 {
        bar = bar.push(Space::new().width(Length::FillPortion(STEPS as u16 - done)));
    }
    let track = fade(color::TRACK, k);
    container(bar)
        .width(Length::Fill)
        .style(move |_| pill(track, Color::TRANSPARENT, None, r))
        .into()
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

// ---------------------------------------------------------------- sheets

/// One of a dialog's choices: `key word`, the pick a gold pill, or a
/// danger one when the choice destroys. Choices share the sheet's width
/// equally.
fn choice(
    key: &'static str,
    word: &'static str,
    on: bool,
    fg: Color,
    press: Message,
    k: f32,
) -> Element<'static, Message> {
    let danger = fg == color::DANGER;
    let fg = if on && !danger {
        color::ACCENT_TEXT
    } else {
        fg
    };
    let mut words = Row::new();
    if !key.is_empty() {
        words = words.push(label(
            format!("{key} "),
            size::TEXT_SMALL,
            fade(color::TEXT_TERTIARY, k),
        ));
    }
    words = words.push(caption(word, size::TEXT, fade(fg, k)).font(if on {
        font::UI_MEDIUM
    } else {
        font::UI
    }));
    let cell = container(words.align_y(alignment::Vertical::Center))
        .padding([size::CHOICE_PAD_Y, size::CHOICE_PAD_X])
        .width(Length::Fill)
        .height(size::ROW_H + size::CHOICE_PAD_Y)
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
        .style(move |_| match (on && k > 0.0, danger) {
            (false, _) => container::Style::default(),
            (true, false) => cursor_style(true),
            (true, true) => pill(
                color::DANGER_FILL,
                color::DANGER_BORDER,
                None,
                look().chip_radius,
            ),
        });
    if on {
        mouse_area(cell).on_press(press).into()
    } else {
        parts::hover(cell, press, 0.0).width(Length::Fill).into()
    }
}

/// Padding around a sheet's section.
fn section<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding([size::CHROME_Y * 1.5, size::SHEET_PAD])
        .width(Length::Fill)
        .into()
}

/// A job found its destination taken: the two files side by side, then
/// the choices. Skip is picked first; `a` applies the answer to the rest
/// of this job's conflicts. `k` fades the content in with its sheet.
fn conflict_dialog<'a>(app: &'a App, c: &'a Conflict, k: f32) -> Element<'a, Message> {
    let lossy = |p: Vec<u8>| String::from_utf8_lossy(&p).into_owned();
    let dir = |p: &[u8]| split_parent(p).map(|(d, _)| lossy(d)).unwrap_or_default();
    let waiting = app.conflicts.len();
    let f = |c: Color| fade(c, k);
    let head = column![
        row![
            container(
                caption(
                    format!("“{}” already exists", basename(&c.dest)),
                    size::TEXT,
                    f(color::TEXT)
                )
                .font(font::UI_SEMIBOLD)
            )
            .width(Length::Fill)
            .clip(true),
            caption(
                if waiting > 1 {
                    format!("1 of {}", group(waiting))
                } else {
                    String::new()
                },
                size::TEXT_SMALL,
                f(color::TEXT_TERTIARY)
            ),
        ],
        container(label(
            format!("in {}", dir(&c.dest)),
            size::TEXT_SMALL,
            f(color::TEXT_TERTIARY)
        ))
        .clip(true),
    ]
    .spacing(size::HAIRLINE * 2.0);
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
                    label(v, size::TEXT_SMALL, f(color::TEXT_SECONDARY)),
                    caption(
                        if tag {
                            format!("  {word}")
                        } else {
                            String::new()
                        },
                        size::TEXT_SMALL,
                        f(color::TEXT)
                    ),
                ]
            };
            container(
                column![
                    caption(title, size::CAPTION, f(color::TEXT_TERTIARY)),
                    // The header names the full folder; each side needs only
                    // its own folder's name to tell the two apart.
                    container(label(
                        format!("{}/", basename(dir(path).as_bytes())),
                        size::TEXT_SMALL,
                        f(color::TEXT_SECONDARY)
                    ))
                    .clip(true),
                    fact(size_s, larger, "larger"),
                    fact(date, newer, "newer"),
                ]
                .spacing(size::HAIRLINE * 2.0),
            )
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
    let mut choices = Row::new().spacing(size::PILL_X);
    for r in c.choices() {
        choices = choices.push(choice(
            resolution_key(r),
            resolution_label(r),
            r == c.pick,
            color::TEXT_SECONDARY,
            Message::Resolve(r),
            k,
        ));
    }
    let footer = row![
        parts::hover(
            row![
                label("a ", size::TEXT_SMALL, f(color::TEXT_TERTIARY)),
                caption(
                    if c.apply_all {
                        "apply to all: on"
                    } else {
                        "apply to all: off"
                    },
                    size::TEXT_SMALL,
                    f(if c.apply_all {
                        color::TEXT
                    } else {
                        color::TEXT_SECONDARY
                    })
                ),
            ]
            .align_y(alignment::Vertical::Center),
            Message::ApplyAll,
            [size::HAIRLINE * 2.0, size::CRUMB_X * 2.0],
        ),
        Space::new().width(Length::Fill),
        caption(
            "enter choose · esc skip",
            size::TEXT_SMALL,
            f(color::TEXT_TERTIARY)
        ),
    ];
    container(column![
        section(head),
        container(rule_h(k)).padding([0.0, size::SHEET_PAD]),
        section(sides),
        container(choices).padding([0.0, size::PANEL_PAD]),
        section(footer),
    ])
    .width(size::DIALOG_W)
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
fn confirm_dialog(c: &Confirm, k: f32) -> Element<'_, Message> {
    let f = |c: Color| fade(c, k);
    let head = column![
        caption(c.headline(), size::TEXT, f(color::TEXT)).font(font::UI_SEMIBOLD),
        container(label(
            c.listing(),
            size::TEXT_SMALL,
            f(color::TEXT_SECONDARY)
        ))
        .clip(true),
        caption(
            "this cannot be undone",
            size::TEXT_SMALL,
            f(color::TEXT_TERTIARY)
        ),
    ]
    .spacing(size::HAIRLINE * 2.0);
    let choices = row![
        choice(
            "esc",
            "cancel",
            !c.delete,
            color::TEXT_SECONDARY,
            Message::Confirm(false),
            k
        ),
        choice(
            "",
            "delete permanently",
            c.delete,
            color::DANGER,
            Message::Confirm(true),
            k
        ),
    ]
    .spacing(size::PILL_X);
    container(column![
        section(head),
        container(choices).padding([0.0, size::PANEL_PAD]),
        section(row![
            Space::new().width(Length::Fill),
            caption(
                "← → choose · enter confirm",
                size::TEXT_SMALL,
                f(color::TEXT_TERTIARY)
            ),
        ]),
    ])
    .width(size::DIALOG_W)
    .into()
}

/// Naming a new folder or file: a small sheet under the top edge, the
/// caret the gold, an error below the line.
fn name_sheet(n: &NameEntry, k: f32) -> Element<'_, Message> {
    let what = if n.what == NameFor::Folder {
        "new folder"
    } else {
        "new file"
    };
    let (hint, hint_fg) = match n.error {
        Some(e) => (e.to_owned(), color::DANGER),
        None => ("enter create · esc cancel".to_owned(), color::TEXT_TERTIARY),
    };
    container(
        column![
            caption(what, size::CAPTION, fade(color::TEXT_TERTIARY, k)),
            field(line_edit(&n.line, color::ACCENT, String::new(), k), k),
            caption(hint, size::TEXT_SMALL, fade(hint_fg, k)),
        ]
        .spacing(size::PILL_X),
    )
    .padding(size::SHEET_PAD)
    .width(size::NAME_W)
    .into()
}

/// An inset input field inside a sheet.
fn field<'a>(content: Element<'a, Message>, k: f32) -> Element<'a, Message> {
    let l = look();
    let (fill, border) = (fade(color::MARK_FILL, k), fade(color::BORDER, k));
    container(content)
        .padding([0.0, size::PAD_X - size::PILL_X])
        .height(size::INPUT_H)
        .width(Length::Fill)
        .align_y(alignment::Vertical::Center)
        .clip(true)
        .style(move |_| pill(fill, border, None, l.inset_radius))
        .into()
}

/// The command palette: a glass sheet under the top edge, the query in an
/// inset field, then the matches with their chords right-aligned. The pick
/// is the gold pill; the caret stays white so the pick keeps the only gold.
fn palette<'a>(app: &'a App, p: &'a Palette, k: f32) -> Element<'a, Message> {
    let f = |c: Color| fade(c, k);
    let items = p.matches(&app.actions);
    let pick = p.pick.min(items.len().saturating_sub(1));
    let start = pick.saturating_sub(size::PALETTE_ROWS - 1);
    let input = field(
        row![
            label(": ", size::TEXT, f(color::TEXT_TERTIARY)),
            line_edit(&p.line, color::TEXT, String::new(), k),
        ]
        .into(),
        k,
    );
    let mut rows = Column::new();
    if items.is_empty() {
        rows = rows.push(
            container(caption(
                "no match",
                size::TEXT_SMALL,
                f(color::TEXT_TERTIARY),
            ))
            .padding([size::CHROME_Y, size::PAD_X - size::PILL_X]),
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
        let fg = if on {
            color::ACCENT_TEXT
        } else {
            color::TEXT_SECONDARY
        };
        let r = container(
            row![
                container(caption(name, size::TEXT, f(fg)))
                    .width(Length::Fill)
                    .clip(true),
                label(hint, size::TEXT_SMALL, f(color::TEXT_TERTIARY)),
            ]
            .align_y(alignment::Vertical::Center),
        )
        .padding([0.0, size::PAD_X - size::PILL_X])
        .width(Length::Fill)
        .height(size::ROW_H)
        .align_y(alignment::Vertical::Center)
        .style(move |_| {
            if on && k > 0.0 {
                cursor_style(true)
            } else {
                container::Style::default()
            }
        });
        // The picked row wears the cursor; the others wash under the
        // pointer.
        rows = rows.push(if on && k > 0.0 {
            Element::from(mouse_area(r).on_press(Message::Run(*item)))
        } else {
            parts::hover(r, Message::Run(*item), Padding::ZERO)
                .width(Length::Fill)
                .into()
        });
    }
    container(column![input, rows].spacing(size::PILL_X))
        .padding(size::PANEL_PAD)
        .width(size::PALETTE_W)
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
    use super::{bytes, crumbs, group, job_amount, job_subject, kind_text};
    use fog_proto::{Entry, Kind};

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

    #[test]
    fn a_new_folder_is_named_not_counted() {
        use crate::ops::Tray;
        use fog_proto::{ConflictPolicy, JobSpec, JobStatus, Reply};
        let now = std::time::Instant::now();
        let mut t = Tray::default();
        t.submit(JobSpec::Mkdir {
            path: b"/w/NewDir".to_vec(),
            on_conflict: ConflictPolicy::Fail,
        });
        t.on_reply(&Reply::JobAccepted { id: 1 }, now);
        t.on_reply(
            &Reply::JobProgress {
                id: 1,
                bytes_done: 0,
                bytes_total: 0,
                files_done: 1,
                files_total: 1,
                files_skipped: 0,
                current: b"/w/NewDir".to_vec(),
            },
            now,
        );
        assert_eq!(job_subject(t.get(1).unwrap()), "NewDir");
        t.on_reply(
            &Reply::JobState {
                id: 1,
                state: JobStatus::Done,
            },
            now,
        );
        let j = t.get(1).unwrap();
        assert_eq!(job_subject(j), "created NewDir");
        assert_eq!(job_amount(j), "");
    }

    #[test]
    fn a_folder_copy_is_named_by_its_source_not_its_last_file() {
        use crate::ops::Tray;
        use fog_proto::{ConflictPolicy, JobSpec, JobStatus, Reply};
        let now = std::time::Instant::now();
        let progress = |id, current: &[u8], skipped| Reply::JobProgress {
            id,
            bytes_done: 10,
            bytes_total: 10,
            files_done: 2,
            files_total: 2,
            files_skipped: skipped,
            current: current.to_vec(),
        };
        let mut t = Tray::default();
        t.submit(JobSpec::Copy {
            srcs: vec![b"/w/bulk".to_vec()],
            dest: b"/d".to_vec(),
            on_conflict: ConflictPolicy::Ask,
        });
        t.on_reply(&Reply::JobAccepted { id: 1 }, now);
        t.on_reply(&progress(1, b"/w/bulk/f00000", 0), now);
        assert_eq!(job_subject(t.get(1).unwrap()), "bulk");
        t.on_reply(
            &Reply::JobState {
                id: 1,
                state: JobStatus::Done,
            },
            now,
        );
        assert_eq!(job_subject(t.get(1).unwrap()), "bulk");
        assert!(!t.get(1).unwrap().skipped_all());

        // Skipped whole: finished, but it says so.
        t.submit(JobSpec::Copy {
            srcs: vec![b"/w/bulk".to_vec()],
            dest: b"/d".to_vec(),
            on_conflict: ConflictPolicy::Ask,
        });
        t.on_reply(&Reply::JobAccepted { id: 2 }, now);
        t.on_reply(&progress(2, b"/w/bulk", 2), now);
        t.on_reply(
            &Reply::JobState {
                id: 2,
                state: JobStatus::Done,
            },
            now,
        );
        assert!(t.get(2).unwrap().skipped_all());
        assert_eq!(job_subject(t.get(2).unwrap()), "bulk");

        // Another client's job: all we know is the path being worked on.
        t.on_reply(&progress(3, b"/x/other.bin", 0), now);
        assert_eq!(job_subject(t.get(3).unwrap()), "other.bin");
    }

    #[test]
    fn the_kind_column_names_types_in_words() {
        let e = |n: &str, k| Entry::new(n.as_bytes().to_vec(), k);
        assert_eq!(kind_text(&e("src", Kind::Dir)), "Folder");
        assert_eq!(kind_text(&e("l", Kind::Symlink)), "Link");
        assert_eq!(kind_text(&e("a.pdf", Kind::File)), "PDF");
        assert_eq!(kind_text(&e("README", Kind::File)), "Document");
        // A file named like a folder's label is still a file.
        assert_eq!(kind_text(&e("x.dir", Kind::File)), "DIR");
    }
}
