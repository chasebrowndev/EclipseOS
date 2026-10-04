// SPDX-License-Identifier: AGPL-3.0-only
//! The sidebar's search: a glass field at the top of the rail, and — while
//! it holds a query — a ranked list of settings in place of the tree.
//!
//! Ranking is `search.rs`; where a hit goes is `app::locate` and
//! `Message::Reveal`. This file is the field, the list, and the keys.
//!
//! Accent ledger: the selected result's 3px bar, the same yellow the tree's
//! active page carries — the tree is not drawn while results are, so the
//! rail still holds exactly one. Everything else is white at 1.0/0.64/0.40.
//!
//! The field and the result row are composed here rather than in
//! `ec_ui::widget::parts` only because this change is confined to this
//! crate; both are candidates to move there (see the notes on each).

use iced::keyboard::{key::Named, Event as KeyEvent, Key as KeyName};
use iced::widget::{button, column, container, row, stack, text, text_input, Column, Space};
use iced::{Alignment, Element, Length, Padding, Subscription, Task, Theme};

use ec_ui::theme;
use ec_ui::tokens::{color, font, radius, size, space};
use ec_ui::widget::{key_hint, key_hints, micro_label, value as mono, Density};

use crate::app::{locate, App, Message};
use crate::pane::place_for;
use crate::schema::Row as Key;
use crate::search::{self, Place, SearchEntry};
use ec_services::frecency::{self, Store};

/// The search field's widget id: what Ctrl+F and type-ahead focus.
pub const FIELD: &str = "settings-search";

/// The index, the query and where the cursor is in its results.
#[derive(Debug, Default)]
pub struct Search {
    query: String,
    entries: Vec<SearchEntry>,
    hits: Vec<(f32, usize)>,
    selected: usize,
    /// Results the human has opened before, read at each reindex; empty with
    /// `settings.search.frecency` off.
    usage: Store,
    /// `settings.search.frecency`, as of the last schema reply.
    frecency: bool,
}

/// The key that turns result history on and off.
const FRECENCY_KEY: &str = "settings.search.frecency";

#[derive(Debug, Clone)]
pub enum Msg {
    /// The field's text changed.
    Query(String),
    /// Ctrl+F: focus the field, its text selected.
    Focus,
    /// Printable text typed while no text field had focus: it starts (or
    /// continues) the query.
    TypeAhead(String),
    /// Arrow keys: move the selected result.
    Move(isize),
    /// Enter: go to the selected result.
    Activate,
    /// A click on result `n`.
    Pick(usize),
    /// Escape: drop the query, bring the tree back.
    Clear,
}

impl Search {
    /// Whether results stand in for the tree.
    pub fn active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Rebuild the index from a fresh schema reply, keeping the query.
    pub fn reindex(&mut self, rows: &[Key]) {
        self.entries = search::index(rows, place);
        self.frecency = rows
            .iter()
            .find(|r| r.path == FRECENCY_KEY)
            .is_none_or(|r| r.value.as_bool().unwrap_or(true));
        // Tests never read the human's real history.
        self.usage = if self.frecency && !cfg!(test) {
            Store::load_file(frecency::SETTINGS_FILE)
        } else {
            Store::default()
        };
        self.rerun();
    }

    /// Drop the remembered results after the history files were cleared, so
    /// the next query ranks as on a first run.
    pub fn forget(&mut self) {
        self.usage = Store::default();
        self.rerun();
    }

    /// Put `query` in the field as if typed. Debug previews use it.
    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.selected = 0;
        self.rerun();
    }

    /// Select result `n` (clamped). Debug previews use it.
    pub fn select(&mut self, n: usize) {
        self.selected = n.min(self.hits.len().saturating_sub(1));
    }

    fn rerun(&mut self) {
        self.hits = search::search_with(
            &self.entries,
            &self.query,
            search::LIMIT,
            &self.usage,
            frecency::now(),
        );
        self.select(self.selected);
    }

    /// Where the selected result goes.
    fn target(&mut self) -> Task<Message> {
        let Some(&(_, i)) = self.hits.get(self.selected) else {
            return Task::none();
        };
        let Some(at) = locate(&self.entries[i].path) else {
            return Task::none();
        };
        self.record(i);
        Task::done(Message::Reveal(at))
    }

    /// Note that result `i` was opened from the search box, so near ties go
    /// its way next time. The path only, never the query; a no-op with the
    /// flag off, and under test so a test never writes the human's history.
    fn record(&mut self, i: usize) {
        if !self.frecency || cfg!(test) {
            return;
        }
        let path = &self.entries[i].path;
        self.usage.record_at(path, frecency::now());
        let entries = &self.entries;
        frecency::record_use(frecency::SETTINGS_FILE, path, |k| {
            entries.iter().any(|e| e.path == k)
        });
    }
}

/// How a schema row reads in a result's breadcrumb: its section, and its
/// page where the section has more than one.
pub fn place(row: &Key) -> Option<Place> {
    let at = place_for(&row.path)?;
    Some(Place {
        section: at.section.title().to_owned(),
        page: at.section.expands().then(|| at.page.title().to_owned()),
        group: None,
    })
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    let s = &mut app.search;
    match msg {
        Msg::Query(q) => s.set_query(&q),
        Msg::Focus => {
            return iced::widget::operation::focus(FIELD).chain(iced::widget::operation::select_all(FIELD));
        }
        Msg::TypeAhead(t) => {
            let q = format!("{}{t}", s.query);
            s.set_query(&q);
            return iced::widget::operation::focus(FIELD)
                .chain(iced::widget::operation::move_cursor_to_end(FIELD));
        }
        Msg::Move(by) => {
            let last = s.hits.len().saturating_sub(1) as isize;
            s.selected = (s.selected as isize + by).clamp(0, last) as usize;
        }
        Msg::Activate => return s.target(),
        Msg::Pick(n) => {
            s.select(n);
            return s.target();
        }
        Msg::Clear => s.set_query(""),
    }
    Task::none()
}

/// The search's keys. Always: Ctrl+F, and printable text no field took.
/// While results show, also the arrows, Enter and Escape — Escape even when
/// the field took it, since the field only drops focus on it. While the
/// field holds only spaces there are no results to move through, but Escape
/// still clears it: any text in the field is Escape's to drop.
pub fn keys(s: &Search) -> Subscription<Message> {
    if s.active() {
        iced::event::listen_with(active_keys)
    } else if !s.query.is_empty() {
        iced::event::listen_with(blank_keys)
    } else {
        iced::event::listen_with(idle_keys)
    }
}

fn blank_keys(event: iced::Event, status: iced::event::Status, _w: iced::window::Id) -> Option<Message> {
    if let Some(m) = always(&event, status) {
        return Some(Message::Search(m));
    }
    match &event {
        iced::Event::Keyboard(KeyEvent::KeyPressed {
            key: KeyName::Named(Named::Escape),
            modifiers,
            ..
        }) if modifiers.is_empty() => Some(Message::Search(Msg::Clear)),
        _ => None,
    }
}

fn idle_keys(event: iced::Event, status: iced::event::Status, _w: iced::window::Id) -> Option<Message> {
    always(&event, status).map(Message::Search)
}

fn active_keys(event: iced::Event, status: iced::event::Status, _w: iced::window::Id) -> Option<Message> {
    if let Some(m) = always(&event, status) {
        return Some(Message::Search(m));
    }
    let iced::Event::Keyboard(KeyEvent::KeyPressed { key, modifiers, .. }) = &event else {
        return None;
    };
    if !modifiers.is_empty() {
        return None;
    }
    let m = match key {
        KeyName::Named(Named::ArrowUp) => Msg::Move(-1),
        KeyName::Named(Named::ArrowDown) => Msg::Move(1),
        KeyName::Named(Named::Escape) => Msg::Clear,
        // In the field, Enter is its `on_submit`; this is Enter elsewhere.
        KeyName::Named(Named::Enter) if status == iced::event::Status::Ignored => Msg::Activate,
        _ => return None,
    };
    Some(Message::Search(m))
}

fn always(event: &iced::Event, status: iced::event::Status) -> Option<Msg> {
    let iced::Event::Keyboard(KeyEvent::KeyPressed {
        key, modifiers, text, ..
    }) = event
    else {
        return None;
    };
    if modifiers.command() && !modifiers.alt() && key.as_ref() == KeyName::Character("f") {
        return Some(Msg::Focus);
    }
    if status != iced::event::Status::Ignored || modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    let t = text.as_deref()?;
    (!t.trim().is_empty() && !t.chars().any(char::is_control)).then(|| Msg::TypeAhead(t.to_owned()))
}

/// The field at the top of the rail.
///
/// The input is its own box — a glass lozenge whose focus is a brighter fill
/// and a crisper rim, never a gold ring — so focus shows on the field itself.
/// With hints on, an empty field carries its Ctrl+F keycap at the right.
pub fn field(app: &App, density: Density) -> Element<'_, Message, Theme> {
    let s = &app.search;
    let input = text_input("Search settings", &s.query)
        .id(FIELD)
        .on_input(|q| Message::Search(Msg::Query(q)))
        .on_submit(Message::Search(Msg::Activate))
        .font(font::UI)
        .size(size::BODY)
        .padding([space::NAV_Y, space::NAV_X])
        .icon(text_input::Icon {
            font: font::DATA_MEDIUM,
            code_point: '/',
            size: Some(size::BODY.into()),
            spacing: space::KEY_GAP,
            side: text_input::Side::Left,
        })
        .style(field_style);
    let cap = app.key_hints_on() && s.query.is_empty() && density == Density::Regular;
    let field: Element<'_, Message, Theme> = if cap {
        stack![
            input,
            container(key_hint("ctrl f", ""))
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(Padding::ZERO.right(space::NAV_X))
                .align_x(Alignment::End)
                .align_y(Alignment::Center),
        ]
        .into()
    } else {
        input.into()
    };
    container(field)
        .width(Length::Fill)
        // In line with the nav and result fills, past the bar column.
        .padding(
            Padding::ZERO
                .left(space::BAR_W + space::NAV_BAR_GAP)
                .bottom(space::ROW_Y),
        )
        .into()
}

/// The field's glass: [`theme::eclipse_input`] with its focus told by fill
/// and rim instead of the accent border, which on the rail would compete
/// with the selected page's bar.
fn field_style(t: &Theme, status: text_input::Status) -> text_input::Style {
    let (fill, rim) = match status {
        text_input::Status::Focused { .. } => (color::HIGHLIGHT_SOFT, color::BORDER_STRONG),
        text_input::Status::Hovered => (color::LIFT_SOFT, color::BORDER_STRONG),
        _ => (color::LIFT_SOFT, color::BORDER),
    };
    text_input::Style {
        background: iced::Background::Color(fill),
        border: iced::Border {
            color: rim,
            width: space::HAIRLINE,
            radius: radius::INSET.into(),
        },
        ..theme::eclipse_input(t, status)
    }
}

/// The rail's items while a query is in: a count, the ranked results (or
/// the empty state), and — with hints on — the keys that drive them.
pub fn results(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let s = &app.search;
    let mut out = Vec::new();
    if s.hits.is_empty() {
        out.push(empty(&s.query));
    } else {
        out.push(
            container(
                row![
                    micro_label("matches"),
                    Space::new().width(Length::Fill),
                    mono(&s.hits.len().to_string()),
                ]
                .align_y(Alignment::Center),
            )
            .padding(
                Padding::ZERO
                    .left(TEXT_INSET)
                    .right(space::NAV_X)
                    .bottom(space::LINE_GAP),
            )
            .into(),
        );
        for (n, &(_, i)) in s.hits.iter().enumerate() {
            out.push(result(&s.entries[i], n == s.selected, n));
        }
    }
    if app.key_hints_on() {
        let mut hints = Column::new().spacing(space::KEY_GAP);
        if !s.hits.is_empty() {
            hints = hints.push(key_hints(&[("↑↓", "move"), ("enter", "open")]));
        }
        hints = hints.push(key_hints(&[("esc", "clear")]));
        out.push(
            container(hints)
                .padding(Padding::ZERO.left(TEXT_INSET).top(space::ROW_Y))
                .into(),
        );
    }
    out
}

/// Where a result's text starts: past the bar column, aligned with the
/// tree's labels.
const TEXT_INSET: f32 = space::BAR_W + space::NAV_BAR_GAP + space::NAV_X;

/// One result: label over its `section › page` breadcrumb, the rail's bar
/// beside it.
///
/// Two lines where a nav row has one, so a list of hits never reads as the
/// tree it replaced. Selected is the sidebar's glassy state — a brighter
/// fill, a crisp rim, the 3px accent bar — and never a glow. A candidate for
/// `parts.rs` beside `nav_page_at`, whose selected state it shares.
fn result(entry: &SearchEntry, selected: bool, n: usize) -> Element<'_, Message, Theme> {
    let face = column![
        text(entry.label.as_str())
            .font(if selected { font::UI_MEDIUM } else { font::UI })
            .size(size::BODY),
        text(entry.breadcrumb.as_str())
            .font(font::DATA)
            .size(size::MICRO)
            .style(theme::text_tertiary),
    ]
    .spacing(space::LINE_GAP);
    row![
        container(Space::new())
            .width(space::BAR_W)
            .height(space::NAV_BAR_H)
            .style(move |_t: &Theme| container::Style {
                background: selected.then_some(iced::Background::Color(color::ACCENT)),
                border: iced::border::rounded(radius::BAR),
                ..container::Style::default()
            }),
        button(face)
            .padding([space::NAV_Y, space::NAV_X])
            .width(Length::Fill)
            .on_press(Message::Search(Msg::Pick(n)))
            .style(result_style(selected)),
    ]
    .spacing(space::NAV_BAR_GAP)
    .align_y(Alignment::Center)
    .into()
}

fn result_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let fill = match (selected, hovered) {
            (true, _) => Some(iced::Background::Color(color::HIGHLIGHT_SOFT)),
            (false, true) => Some(iced::Background::Color(color::LIFT_SOFT)),
            (false, false) => None,
        };
        button::Style {
            background: fill,
            text_color: if selected || hovered {
                color::TEXT
            } else {
                color::TEXT_SECONDARY
            },
            border: iced::Border {
                color: if selected {
                    color::BORDER
                } else {
                    iced::Color::TRANSPARENT
                },
                width: space::HAIRLINE,
                radius: radius::INSET.into(),
            },
            ..button::Style::default()
        }
    }
}

/// Nothing matched: say so, and what to try.
fn empty(query: &str) -> Element<'_, Message, Theme> {
    container(
        column![
            micro_label("no match"),
            text(format!(
                "Nothing in Settings matches \u{201c}{}\u{201d}.",
                query.trim()
            ))
            .font(font::UI)
            .size(size::BODY_SMALL)
            .style(theme::text_secondary),
            text("Try a plainer word: blur, keyboard, wifi.")
                .font(font::UI)
                .size(size::BODY_SMALL)
                .style(theme::text_tertiary),
        ]
        .spacing(space::KEY_GAP),
    )
    .padding(Padding::ZERO.left(TEXT_INSET).right(space::NAV_X))
    .into()
}
