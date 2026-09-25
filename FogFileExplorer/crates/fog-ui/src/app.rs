// SPDX-License-Identifier: AGPL-3.0-only

//! The M0 window: path bar, the virtualized listing, status line
//! (FOG §UI and navigation).
//!
//! Composition: the listing is the hero and takes every spare pixel; the path
//! bar above and the status line below are thin mono rows on a slightly
//! lifted ground, separated from it by hairline rules. The one accented
//! value is the selected row (gold bar, gold name, faint gold ground).
//! Nothing else is ever gold.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::time::Duration;

use fog_proto::{Entry, Kind, Request};
use fog_widgets::{scroll_into_view, virtual_list};
use iced::keyboard::{self, key::Named, Key};
use iced::widget::operation::{scroll_to, AbsoluteOffset};
use iced::widget::text::Wrapping;
use iced::widget::{column, container, row, text, Id, Space};
use iced::{alignment, Background, Border, Color, Element, Length, Subscription, Task};

use crate::conn::{self, Link};
use crate::state::{Browser, Effect};
use crate::theme::{color, size};

fn list_id() -> Id {
    Id::new("fog-list")
}

pub struct App {
    browser: Browser,
    link: Option<Link>,
    fogd: Fogd,
    /// Keys replayed one per [`SCRIPT_TICK`], for screenshots and probing
    /// on hosts without input injection. Filled from `FOG_UI_SCRIPT` in
    /// debug builds only; always empty in release.
    script: VecDeque<Key>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fogd {
    Connecting,
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub enum Message {
    Key(keyboard::Event),
    Conn(conn::Event),
    Script,
}

const SCRIPT_TICK: Duration = Duration::from_millis(900);

/// `FOG_UI_SCRIPT="j j l . h G"`: space-separated keys. One character is
/// that key; `Enter`, `Backspace`, `Up`, `Down` are named keys; `.` waits a
/// tick. Debug builds only.
fn script() -> VecDeque<Key> {
    if !cfg!(debug_assertions) {
        return VecDeque::new();
    }
    let raw = std::env::var("FOG_UI_SCRIPT").unwrap_or_default();
    raw.split_whitespace()
        .map(|t| match t {
            "Enter" => Key::Named(Named::Enter),
            "Backspace" => Key::Named(Named::Backspace),
            "Up" => Key::Named(Named::ArrowUp),
            "Down" => Key::Named(Named::ArrowDown),
            t => Key::Character(t.into()),
        })
        .collect()
}

impl App {
    pub fn new(path: Vec<u8>) -> Self {
        Self {
            browser: Browser::new(path),
            link: None,
            fogd: Fogd::Connecting,
            script: script(),
        }
    }

    fn request(&mut self, path: Vec<u8>) {
        if let Some(link) = &self.link {
            if link.send(Request::ListDir { path }).is_err() {
                self.link = None;
            }
        }
        // Offline: the target is re-requested when the link comes up.
    }

    fn apply(&mut self, fx: Effect) -> Task<Message> {
        match fx {
            Effect::None => Task::none(),
            Effect::Reveal(i) => scroll_into_view(list_id(), i, size::ROW_H),
            Effect::Entered(i) => scroll_to(list_id(), AbsoluteOffset { x: 0.0, y: 0.0 })
                .chain(scroll_into_view(list_id(), i, size::ROW_H)),
            Effect::List(path) => {
                self.request(path);
                Task::none()
            }
        }
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    let fx = match message {
        Message::Conn(conn::Event::Up(link)) => {
            app.link = Some(link);
            app.fogd = Fogd::Up;
            Effect::List(app.browser.target().to_vec())
        }
        Message::Conn(conn::Event::Down) => {
            app.link = None;
            app.fogd = Fogd::Down;
            Effect::None
        }
        Message::Conn(conn::Event::Reply(r)) => app.browser.on_reply(r),
        Message::Key(keyboard::Event::KeyPressed { key, .. }) => press(&mut app.browser, &key),
        Message::Key(_) => Effect::None,
        Message::Script => match app.script.pop_front() {
            Some(key) => press(&mut app.browser, &key),
            None => Effect::None,
        },
    };
    app.apply(fx)
}

/// The M0 key map, hard-coded (config bindings are M1).
fn press(b: &mut Browser, key: &Key) -> Effect {
    match key.as_ref() {
        Key::Character("j") | Key::Named(Named::ArrowDown) => b.step(1),
        Key::Character("k") | Key::Named(Named::ArrowUp) => b.step(-1),
        Key::Character("l") | Key::Named(Named::Enter) => b.open(),
        Key::Character("h") | Key::Named(Named::Backspace) => b.parent(),
        Key::Character("g") => b.first(),
        Key::Character("G") => b.last(),
        _ => Effect::None,
    }
}

pub fn subscription(app: &App) -> Subscription<Message> {
    let base = [
        keyboard::listen().map(Message::Key),
        conn::subscription().map(Message::Conn),
    ];
    if app.script.is_empty() {
        Subscription::batch(base)
    } else {
        Subscription::batch(
            base.into_iter()
                .chain([iced::time::every(SCRIPT_TICK).map(|_| Message::Script)]),
        )
    }
}

pub fn view(app: &App) -> Element<'_, Message> {
    let b = &app.browser;
    let selected = b.selected;
    let body: Element<'_, Message> = if b.len() == 0 && b.complete && b.pending.is_none() {
        // A listed folder with nothing in it says so, quietly.
        container(label(
            "empty folder",
            size::TEXT_SMALL,
            color::TEXT_TERTIARY,
        ))
        .center(Length::Fill)
        .into()
    } else {
        virtual_list(b.len(), size::ROW_H, move |i| {
            list_row(b.row(i), i == selected)
        })
        .id(list_id())
        .into()
    };

    container(column![
        chrome(path_bar(app), Edge::Bottom),
        container(body).height(Length::Fill),
        chrome(status_line(app), Edge::Top),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_| container::Style {
        background: Some(Background::Color(color::BASE)),
        text_color: Some(color::TEXT),
        ..Default::default()
    })
    .into()
}

fn label<'a>(s: impl text::IntoFragment<'a>, sz: f32, c: Color) -> text::Text<'a> {
    text(s).size(sz).color(c).wrapping(Wrapping::None)
}

/// Breadcrumbs: ancestors tertiary, the current folder primary. While a
/// navigation is in flight the requested path is shown, so the bar answers
/// "where am I going" the moment a key is pressed.
fn path_bar(app: &App) -> Element<'_, Message> {
    let path = String::from_utf8_lossy(app.browser.target());
    let (head, tail) = match path.rfind('/') {
        Some(0) if path.len() == 1 => (Cow::Borrowed(""), path.clone()),
        Some(i) => (
            Cow::Owned(path[..=i].to_owned()),
            Cow::Owned(path[i + 1..].to_owned()),
        ),
        None => (Cow::Borrowed(""), path.clone()),
    };
    container(row![
        label(head, size::TEXT, color::TEXT_TERTIARY),
        label(tail, size::TEXT, color::TEXT),
    ])
    .width(Length::Fill)
    .clip(true)
    .into()
}

fn status_line(app: &App) -> Element<'_, Message> {
    let b = &app.browser;
    let count = match b.len() {
        1 => "1 item".to_owned(),
        n => format!("{} items", group(n)),
    };
    let state = if b.pending.is_some() || !b.complete {
        "listing…"
    } else {
        "complete"
    };
    let mut left = row![
        label(count, size::TEXT_SMALL, color::TEXT_SECONDARY),
        label(state, size::TEXT_SMALL, color::TEXT_TERTIARY),
    ]
    .spacing(size::GAP);
    if let Some((path, errno)) = &b.error {
        let msg = std::io::Error::from_raw_os_error(*errno).to_string();
        let msg = msg.split(" (os error").next().unwrap_or(&msg).to_owned();
        left = left.push(label(
            format!("{msg} — {}", String::from_utf8_lossy(path)),
            size::TEXT_SMALL,
            color::DANGER,
        ));
    }
    let (fogd, fogd_color) = match app.fogd {
        Fogd::Connecting => ("fogd …", color::TEXT_TERTIARY),
        Fogd::Up => ("fogd", color::TEXT_TERTIARY),
        Fogd::Down => ("fogd not running", color::DANGER),
    };
    let position = if b.len() == 0 {
        String::new()
    } else {
        format!("{} / {}", group(b.selected + 1), group(b.len()))
    };
    row![
        container(left).width(Length::Fill).clip(true),
        row![
            label(position, size::TEXT_SMALL, color::TEXT_TERTIARY),
            label(fogd, size::TEXT_SMALL, fogd_color),
        ]
        .spacing(size::GAP),
    ]
    .into()
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
        .style(|_| container::Style {
            background: Some(Background::Color(color::CHROME)),
            ..Default::default()
        });
    let rule = container(Space::new())
        .width(Length::Fill)
        .height(size::HAIRLINE)
        .style(|_| container::Style {
            background: Some(Background::Color(color::RULE)),
            ..Default::default()
        });
    match edge {
        Edge::Top => column![rule, body].into(),
        Edge::Bottom => column![body, rule].into(),
    }
}

fn kind_tag(kind: Kind) -> &'static str {
    match kind {
        Kind::Symlink => "link",
        Kind::Other => "other",
        // Folders are already marked by their trailing `/`.
        Kind::Dir | Kind::File | Kind::Unknown => "",
    }
}

/// One listing row: `[bar] name[/]  …  kind`. Folders read primary with a
/// tertiary trailing `/`; files read secondary. Selection is the pane's one
/// gold value.
fn list_row(entry: Option<&Entry>, selected: bool) -> Element<'static, Message> {
    let Some(entry) = entry else {
        return Space::new().into();
    };
    let is_dir = entry.kind == Kind::Dir;
    let name_color = match (selected, is_dir) {
        (true, _) => color::ACCENT_TEXT,
        (false, true) => color::TEXT,
        (false, false) => color::TEXT_SECONDARY,
    };
    let mut name = row![label(entry.display().into_owned(), size::TEXT, name_color)];
    if is_dir {
        name = name.push(label("/", size::TEXT, color::TEXT_TERTIARY));
    }
    let bar = container(Space::new())
        .width(size::BAR_W)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: selected.then_some(Background::Color(color::ACCENT)),
            ..Default::default()
        });
    let body = row![
        container(name).width(Length::Fill).clip(true),
        label(kind_tag(entry.kind), size::TEXT_SMALL, color::TEXT_TERTIARY)
            .width(size::TAG_W)
            .align_x(alignment::Horizontal::Right),
    ]
    .align_y(alignment::Vertical::Center)
    .height(Length::Fill);
    container(row![
        bar,
        container(body)
            .padding([0.0, size::PAD_X - size::BAR_W])
            .width(Length::Fill)
            .height(Length::Fill),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| container::Style {
        background: selected.then_some(Background::Color(color::ACCENT_FILL)),
        border: Border::default(),
        ..Default::default()
    })
    .into()
}

/// `100002` as `100,002`.
fn group(n: usize) -> String {
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

#[cfg(test)]
mod tests {
    #[test]
    fn group_thousands() {
        assert_eq!(super::group(0), "0");
        assert_eq!(super::group(999), "999");
        assert_eq!(super::group(1000), "1,000");
        assert_eq!(super::group(100_002), "100,002");
        assert_eq!(super::group(1_234_567), "1,234,567");
    }
}
