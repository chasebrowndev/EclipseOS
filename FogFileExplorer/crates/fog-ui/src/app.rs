// SPDX-License-Identifier: AGPL-3.0-only

//! The M0 window: path bar, the virtualized listing, status line
//! (FOG §UI and navigation).
//!
//! Composition: the listing is the hero and takes every spare pixel; the path
//! bar above and the status line below are thin mono rows on a slightly
//! lifted ground, separated from it by hairline rules. The one accented
//! value is the selected row (gold bar, gold name, faint gold ground).
//! Nothing else is ever gold.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use fog_config::{Action, Chord, Key as K, Mods, Target};
use fog_proto::{Entry, Kind, Request};
use fog_widgets::{scroll_into_view, virtual_list};
use iced::keyboard::{self, key::Named, Key, Modifiers};
use iced::widget::operation::{scroll_to, AbsoluteOffset};
use iced::widget::responsive;
use iced::widget::text::Wrapping;
use iced::widget::{column, container, row, text, Id, Space};
use iced::{
    alignment, window, Background, Border, Color, Element, Length, Size, Subscription, Task,
};

use crate::conn::{self, Link};
use crate::state::{Browser, Effect};
use crate::theme::{color, size};

fn list_id() -> Id {
    Id::new("fog-list")
}

pub struct App {
    browser: Browser,
    /// `keys { bind … }` from fog.kdl, loaded before iced starts.
    keys: BTreeMap<Chord, Target>,
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
    Resized,
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
    pub fn new(path: Vec<u8>, keys: BTreeMap<Chord, Target>) -> Self {
        Self {
            browser: Browser::new(path),
            keys,
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
            app.browser.reconnected();
            Effect::List(app.browser.target().to_vec())
        }
        Message::Conn(conn::Event::Down) => {
            app.link = None;
            app.fogd = Fogd::Down;
            Effect::None
        }
        Message::Conn(conn::Event::Reply(r)) => app.browser.on_reply(r),
        Message::Key(keyboard::Event::KeyPressed {
            key,
            modified_key,
            modifiers,
            ..
        }) => match chord(&key, &modified_key, modifiers) {
            Some(c) => press(&mut app.browser, &app.keys, c),
            None => Effect::None,
        },
        Message::Key(_) => Effect::None,
        // A shrinking window can leave the selection below the fold.
        Message::Resized if app.browser.len() > 0 => Effect::Reveal(app.browser.selected),
        Message::Resized => Effect::None,
        Message::Script => match app.script.pop_front() {
            Some(key) => match chord(&key, &key, Modifiers::empty()) {
                Some(c) => press(&mut app.browser, &app.keys, c),
                None => Effect::None,
            },
            None => Effect::None,
        },
    };
    app.apply(fx)
}

/// Run the action bound to `c`. Actions the window has no behaviour for
/// yet are ignored.
fn press(b: &mut Browser, keys: &BTreeMap<Chord, Target>, c: Chord) -> Effect {
    match keys.get(&c) {
        Some(Target::Action(Action::Down)) => b.step(1),
        Some(Target::Action(Action::Up)) => b.step(-1),
        Some(Target::Action(Action::Open)) => b.open(),
        Some(Target::Action(Action::Parent)) => b.parent(),
        Some(Target::Action(Action::Top)) => b.first(),
        Some(Target::Action(Action::Bottom)) => b.last(),
        _ => Effect::None,
    }
}

/// The config chord for a key press. With Ctrl, Alt or Super held the
/// unmodified key names it (`ctrl+shift+n`, not a control character);
/// otherwise the produced character does, so `G` and `:` read as typed.
fn chord(key: &Key, modified: &Key, m: Modifiers) -> Option<Chord> {
    let mods = Mods {
        ctrl: m.control(),
        alt: m.alt(),
        shift: m.shift(),
        logo: m.logo(),
    };
    let k = match (
        mods.ctrl || mods.alt || mods.logo,
        key.as_ref(),
        modified.as_ref(),
    ) {
        (true, Key::Character(s), _) | (false, _, Key::Character(s)) => {
            let mut cs = s.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => K::Char(c),
                _ => return None,
            }
        }
        (_, Key::Named(n), _) | (_, _, Key::Named(n)) => match n {
            Named::Enter => K::Enter,
            Named::Backspace => K::Backspace,
            Named::Tab => K::Tab,
            Named::Space => K::Space,
            Named::Delete => K::Delete,
            Named::Insert => K::Insert,
            Named::Home => K::Home,
            Named::End => K::End,
            Named::PageUp => K::PageUp,
            Named::PageDown => K::PageDown,
            Named::ArrowUp => K::Up,
            Named::ArrowDown => K::Down,
            Named::ArrowLeft => K::Left,
            Named::ArrowRight => K::Right,
            Named::F1 => K::F(1),
            Named::F2 => K::F(2),
            Named::F3 => K::F(3),
            Named::F4 => K::F(4),
            Named::F5 => K::F(5),
            Named::F6 => K::F(6),
            Named::F7 => K::F(7),
            Named::F8 => K::F(8),
            Named::F9 => K::F(9),
            Named::F10 => K::F(10),
            Named::F11 => K::F(11),
            Named::F12 => K::F(12),
            _ => return None,
        },
        _ => return None,
    };
    Some(Chord::new(mods, k))
}

pub fn subscription(app: &App) -> Subscription<Message> {
    let base = [
        keyboard::listen().map(Message::Key),
        conn::subscription().map(Message::Conn),
        window::resize_events().map(|_| Message::Resized),
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
/// "where am I going" the moment a key is pressed. When the bar is too
/// narrow the path is elided from the left, so the current folder stays.
fn path_bar(app: &App) -> Element<'_, Message> {
    let path = String::from_utf8_lossy(app.browser.target()).into_owned();
    responsive(move |room: Size| {
        // The UI font is monospace, so a width is a character count.
        let fits = (room.width / (size::TEXT * size::MONO_ADVANCE)) as usize;
        let (head, tail) = elide_left(&path, fits);
        container(row![
            label(head, size::TEXT, color::TEXT_TERTIARY),
            label(tail, size::TEXT, color::TEXT),
        ])
        .width(Length::Fill)
        .clip(true)
        .into()
    })
    .height(Length::Shrink)
    .into()
}

/// `path` split into ancestors and the current folder, elided from the left
/// to fit `fits` characters: `/a/b/c/cur` becomes `…/c/cur`, cut at a `/`
/// where possible, and only the current name's own tail if nothing else fits.
fn elide_left(path: &str, fits: usize) -> (String, String) {
    let (head, tail) = match path.rfind('/') {
        Some(0) if path.len() == 1 => ("", path),
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let tail_n = tail.chars().count();
    if head.chars().count() + tail_n <= fits {
        return (head.to_owned(), tail.to_owned());
    }
    // `…` plus the longest `/`-led suffix of the ancestors that still fits.
    let room = fits.saturating_sub(tail_n + 1);
    let cut = head
        .char_indices()
        .filter(|&(i, c)| c == '/' && i > 0)
        .map(|(i, _)| i)
        .find(|&i| head[i..].chars().count() <= room);
    if let Some(i) = cut {
        return (format!("…{}", &head[i..]), tail.to_owned());
    }
    let keep = fits.saturating_sub(1);
    let skip = tail_n.saturating_sub(keep);
    (
        String::new(),
        format!("…{}", tail.chars().skip(skip).collect::<String>()),
    )
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
    // The left side yields and clips; the counter and fogd state never do.
    row![
        container(left).width(Length::Fill).clip(true),
        row![
            label(position, size::TEXT_SMALL, color::TEXT_TERTIARY),
            label(fogd, size::TEXT_SMALL, fogd_color),
        ]
        .spacing(size::GAP),
    ]
    .spacing(size::GAP)
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
    fn key_presses_resolve_through_the_default_bindings() {
        use super::{chord, Key, Modifiers, Named};
        use fog_config::{Action, Target};
        let keys = fog_config::defaults().keys;
        let hit = |k: Key, m: Key, mods| keys.get(&chord(&k, &m, mods).unwrap()).cloned();
        let act = |a| Some(Target::Action(a));
        let ch = |s: &str| Key::Character(s.into());
        assert_eq!(hit(ch("g"), ch("G"), Modifiers::SHIFT), act(Action::Bottom));
        assert_eq!(hit(ch("g"), ch("g"), Modifiers::empty()), act(Action::Top));
        assert_eq!(
            hit(ch(";"), ch(":"), Modifiers::SHIFT),
            act(Action::Palette)
        );
        let n = Modifiers::CTRL | Modifiers::SHIFT;
        assert_eq!(hit(ch("n"), ch("\u{e}"), n), act(Action::NewFolder));
        let del = Key::Named(Named::Delete);
        assert_eq!(
            hit(del.clone(), del.clone(), Modifiers::SHIFT),
            act(Action::Delete)
        );
        let down = Key::Named(Named::ArrowDown);
        assert_eq!(
            hit(down.clone(), down, Modifiers::empty()),
            act(Action::Down)
        );
    }

    #[test]
    fn path_elides_from_the_left() {
        use super::elide_left;
        let own = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(elide_left("/", 10), own("", "/"));
        assert_eq!(elide_left("/home/u/src", 40), own("/home/u/", "src"));
        // 11 chars do not fit in 9: keep "…/u/src".
        assert_eq!(elide_left("/home/u/src", 9), own("…/u/", "src"));
        assert_eq!(elide_left("/home/u/src", 6), own("…/", "src"));
        // Not even "…/src": the name's own tail.
        assert_eq!(elide_left("/home/u/src", 3), own("", "…rc"));
        assert_eq!(elide_left("/home/u/src", 0), own("", "…"));
    }

    #[test]
    fn group_thousands() {
        assert_eq!(super::group(0), "0");
        assert_eq!(super::group(999), "999");
        assert_eq!(super::group(1000), "1,000");
        assert_eq!(super::group(100_002), "100,002");
        assert_eq!(super::group(1_234_567), "1,234,567");
    }
}
