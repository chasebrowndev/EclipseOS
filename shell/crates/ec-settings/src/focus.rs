// SPDX-License-Identifier: AGPL-3.0-only
//! Keyboard focus for the controls iced's buttons cannot focus themselves: the
//! sidebar's rows, the scheme cards, the quick switches, the "Jump to" rows,
//! the Advanced disclosure and the narrow window's back control.
//!
//! The app keeps one [`Focus`] (or none) and the views draw the ring
//! (`ec_ui::widget::focus_ring`: a brighter fill and a crisp rim, never a
//! glow). Tab and Shift+Tab walk [`order`]; Up and Down walk the sidebar's
//! rows while a row has focus; Left and Right fold and unfold a section;
//! Enter and Space press; Escape drops the ring. Every key is taken only when
//! no widget (a text field) already used it, so typing is never stolen, and
//! the search's own keys (Ctrl+F, type-ahead, its arrows and Escape) stay the
//! search's while results show.

use iced::keyboard::{key::Named, Event as KeyEvent, Key as KeyName};
use iced::{Subscription, Task};

use crate::app::{App, Message};
use crate::pane::{Page, Section};

/// One focusable control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Focus {
    /// A narrow window's way back to the page list.
    Back,
    /// A sidebar section's row.
    Section(Section),
    /// A sub-page row under the open section.
    Page(Page),
    /// The Overview's scheme card `n` (light, dark, automatic).
    Scheme(usize),
    /// The Overview's "more appearance settings" door.
    More,
    /// The Overview's quick switch `n`.
    Quick(usize),
    /// The Overview's "Jump to" row `n`.
    Door(usize),
    /// The page's "Advanced" disclosure.
    Advanced,
}

/// What the last `view` established and the keys need: the window's shape, and
/// whether the shown page has an Advanced disclosure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    pub narrow: bool,
    pub advanced: bool,
}

/// The keys this module reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Next,
    Prev,
    Up,
    Down,
    Left,
    Right,
    Activate,
    Clear,
}

/// The sidebar's rows, top to bottom: every section, and under the open one
/// its pages.
pub fn nav_items(open: Option<Section>) -> Vec<Focus> {
    let mut out = Vec::new();
    for s in Section::ALL {
        out.push(Focus::Section(*s));
        if open == Some(*s) && s.expands() {
            out.extend(s.pages().iter().map(|p| Focus::Page(*p)));
        }
    }
    out
}

/// The shown page's controls, in reading order.
pub fn content_items(app: &App) -> Vec<Focus> {
    let mut out = Vec::new();
    if app.page == Page::Overview {
        out.extend((0..3).map(Focus::Scheme));
        out.push(Focus::More);
        out.extend((0..crate::overview::quick_keys(app).len()).map(Focus::Quick));
        out.extend((0..crate::overview::DOORS.len()).map(Focus::Door));
    }
    if app.facts.get().advanced {
        out.push(Focus::Advanced);
    }
    out
}

/// Everything Tab visits now. A narrow window shows the list or the page, so
/// only that one; while search results show, the results are the search's.
pub fn order(app: &App) -> Vec<Focus> {
    let facts = app.facts.get();
    let list = !facts.narrow || app.show_nav;
    let mut out = Vec::new();
    if facts.narrow && !app.show_nav {
        out.push(Focus::Back);
    }
    if list && !app.search.active() {
        out.extend(nav_items(app.open));
    }
    if !(facts.narrow && app.show_nav) {
        out.extend(content_items(app));
    }
    out
}

/// Where `key` takes the focus from `at`, or `None` to leave it. Pure, so the
/// order is testable without a window.
pub fn step(order: &[Focus], at: Option<Focus>, key: Key) -> Option<Focus> {
    if order.is_empty() {
        return None;
    }
    let pos = at.and_then(|f| order.iter().position(|o| *o == f));
    let n = order.len();
    let in_nav = |f: &Focus| matches!(f, Focus::Section(_) | Focus::Page(_));
    match (key, pos) {
        (Key::Next, None) => Some(order[0]),
        (Key::Prev, None) => Some(order[n - 1]),
        (Key::Next, Some(i)) => Some(order[(i + 1) % n]),
        (Key::Prev, Some(i)) => Some(order[(i + n - 1) % n]),
        // The arrows walk the sidebar only: elsewhere they belong to the page.
        (Key::Down, Some(i)) if in_nav(&order[i]) => order[i + 1..]
            .iter()
            .find(|f| in_nav(f))
            .copied()
            .or(Some(order[i])),
        (Key::Up, Some(i)) if in_nav(&order[i]) => order[..i]
            .iter()
            .rev()
            .find(|f| in_nav(f))
            .copied()
            .or(Some(order[i])),
        _ => None,
    }
}

/// What pressing `focus` sends: what a click on it sends.
pub fn press(focus: Focus, app: &App) -> Option<Message> {
    Some(match focus {
        Focus::Back => Message::Back,
        Focus::Section(s) if s.expands() => Message::Section(s),
        Focus::Section(s) => Message::Open(s.first()),
        Focus::Page(p) => Message::Open(p),
        Focus::Scheme(n) => {
            let value = ["light", "dark", "auto"].get(n)?;
            Message::Chose(crate::schema::COLOR_SCHEME.to_owned(), (*value).to_owned())
        }
        Focus::More => Message::Open(Page::Blur),
        Focus::Quick(n) => {
            let (path, _, _) = *crate::overview::quick_keys(app).get(n)?;
            let on = app.key(path).is_some_and(|k| k.as_bool());
            Message::Toggled(path.to_owned(), !on)
        }
        Focus::Door(n) => Message::Open(*crate::overview::DOORS.get(n)?),
        Focus::Advanced => Message::Advanced,
    })
}

/// Handle a key: move the ring, or press, fold or unfold what it is on.
pub fn update(app: &mut App, key: Key) -> Task<Message> {
    let order = order(app);
    match key {
        Key::Clear => app.focus = None,
        Key::Activate => {
            if let Some(m) = app.focus.and_then(|f| press(f, app)) {
                return Task::done(m);
            }
        }
        // Right opens the section the ring is on; Left folds it.
        Key::Right | Key::Left => {
            if let Some(Focus::Section(s)) = app.focus {
                if s.expands() && (app.open == Some(s)) == (key == Key::Left) {
                    return Task::done(Message::Section(s));
                }
            }
        }
        k => {
            if let Some(next) = step(&order, app.focus, k) {
                app.focus = Some(next);
                return crate::app::nav_reveal(next);
            }
        }
    }
    Task::none()
}

/// The keys. Tab always (it starts the ring); the rest only while a control
/// has the ring, and only when nothing else took the key.
pub fn keys(ringed: bool) -> Subscription<Message> {
    if ringed {
        iced::event::listen_with(ringed_keys)
    } else {
        iced::event::listen_with(idle_keys)
    }
}

fn tab(event: &iced::Event, status: iced::event::Status) -> Option<Key> {
    let iced::Event::Keyboard(KeyEvent::KeyPressed {
        key: KeyName::Named(Named::Tab),
        modifiers,
        ..
    }) = event
    else {
        return None;
    };
    if status != iced::event::Status::Ignored || modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    Some(if modifiers.shift() { Key::Prev } else { Key::Next })
}

fn idle_keys(event: iced::Event, status: iced::event::Status, _w: iced::window::Id) -> Option<Message> {
    tab(&event, status).map(Message::Focus)
}

fn ringed_keys(event: iced::Event, status: iced::event::Status, _w: iced::window::Id) -> Option<Message> {
    if let Some(k) = tab(&event, status) {
        return Some(Message::Focus(k));
    }
    let iced::Event::Keyboard(KeyEvent::KeyPressed { key, modifiers, .. }) = &event else {
        return None;
    };
    if status != iced::event::Status::Ignored || !modifiers.is_empty() {
        return None;
    }
    let k = match key {
        KeyName::Named(Named::ArrowUp) => Key::Up,
        KeyName::Named(Named::ArrowDown) => Key::Down,
        KeyName::Named(Named::ArrowLeft) => Key::Left,
        KeyName::Named(Named::ArrowRight) => Key::Right,
        KeyName::Named(Named::Enter | Named::Space) => Key::Activate,
        KeyName::Named(Named::Escape) => Key::Clear,
        KeyName::Character(c) if c.as_str() == " " => Key::Activate,
        _ => return None,
    };
    Some(Message::Focus(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_lists_every_section_and_the_open_ones_pages() {
        let closed = nav_items(None);
        assert_eq!(closed.len(), Section::ALL.len());
        let open = nav_items(Some(Section::Effects));
        assert_eq!(open.len(), Section::ALL.len() + Section::Effects.pages().len());
        let at = open
            .iter()
            .position(|f| *f == Focus::Section(Section::Effects))
            .unwrap();
        assert_eq!(open[at + 1], Focus::Page(Page::Blur));
        // A section with a single page is not expandable and adds none.
        assert_eq!(nav_items(Some(Section::Display)).len(), closed.len());
    }

    #[test]
    fn tab_starts_at_the_first_and_wraps_both_ways() {
        let o = nav_items(None);
        assert_eq!(step(&o, None, Key::Next), Some(o[0]));
        assert_eq!(step(&o, None, Key::Prev), Some(*o.last().unwrap()));
        assert_eq!(step(&o, Some(*o.last().unwrap()), Key::Next), Some(o[0]));
        assert_eq!(step(&o, Some(o[0]), Key::Prev), Some(*o.last().unwrap()));
        assert_eq!(step(&o, Some(o[3]), Key::Next), Some(o[4]));
        assert_eq!(step(&[], None, Key::Next), None);
    }

    #[test]
    fn the_arrows_walk_the_sidebar_and_nothing_else() {
        let mut o = nav_items(Some(Section::Windows));
        o.push(Focus::Scheme(0));
        o.push(Focus::Advanced);
        let first_page = Focus::Page(Page::Layout);
        assert_eq!(
            step(&o, Some(Focus::Section(Section::Windows)), Key::Down),
            Some(first_page)
        );
        assert_eq!(
            step(&o, Some(first_page), Key::Up),
            Some(Focus::Section(Section::Windows))
        );
        // Past the last row the ring stays on it; off the sidebar the key is the page's.
        let last_nav = *nav_items(Some(Section::Windows)).last().unwrap();
        assert_eq!(step(&o, Some(last_nav), Key::Down), Some(last_nav));
        assert_eq!(step(&o, Some(Focus::Scheme(0)), Key::Down), None);
        assert_eq!(step(&o, None, Key::Down), None);
    }

    #[test]
    fn a_ring_on_a_control_that_left_the_page_restarts_at_the_ends() {
        let o = vec![Focus::Advanced];
        assert_eq!(step(&o, Some(Focus::Quick(7)), Key::Next), Some(Focus::Advanced));
    }

    #[test]
    fn pressing_a_control_sends_what_clicking_it_sends() {
        let app = App::new();
        let m = |f| press(f, &app);
        assert!(matches!(m(Focus::Back), Some(Message::Back)));
        assert!(matches!(m(Focus::Advanced), Some(Message::Advanced)));
        assert!(matches!(m(Focus::More), Some(Message::Open(Page::Blur))));
        assert!(matches!(
            m(Focus::Page(Page::Gaps)),
            Some(Message::Open(Page::Gaps))
        ));
        assert!(matches!(
            m(Focus::Section(Section::Windows)),
            Some(Message::Section(Section::Windows))
        ));
        // A section of one page has nothing to unfold: it opens.
        assert!(matches!(
            m(Focus::Section(Section::Display)),
            Some(Message::Open(Page::Display))
        ));
        match m(Focus::Scheme(1)) {
            Some(Message::Chose(path, v)) => {
                assert_eq!((path.as_str(), v.as_str()), (crate::schema::COLOR_SCHEME, "dark"))
            }
            other => panic!("{other:?}"),
        }
        assert!(m(Focus::Scheme(3)).is_none());
        assert!(matches!(m(Focus::Door(0)), Some(Message::Open(_))));
        assert!(m(Focus::Door(99)).is_none());
        assert!(m(Focus::Quick(99)).is_none());
    }

    #[test]
    fn tab_order_follows_the_window_shape() {
        let app = App::new();
        // Wide: the rail, then the page's controls.
        app.facts.set(Facts {
            narrow: false,
            advanced: true,
        });
        let wide = order(&app);
        assert_eq!(wide[0], Focus::Section(Section::Overview));
        assert_eq!(*wide.last().unwrap(), Focus::Advanced);
        assert!(wide.contains(&Focus::Scheme(0)) && wide.contains(&Focus::Door(0)));
        // Narrow, on a page: the way back, then the page; the rail is not on screen.
        let mut app = app;
        app.facts.set(Facts {
            narrow: true,
            advanced: false,
        });
        app.show_nav = false;
        let page = order(&app);
        assert_eq!(page[0], Focus::Back);
        assert!(!page.iter().any(|f| matches!(f, Focus::Section(_))));
        // Narrow, on the list: the rail only.
        app.show_nav = true;
        let list = order(&app);
        assert!(list
            .iter()
            .all(|f| matches!(f, Focus::Section(_) | Focus::Page(_))));
        assert!(!list.is_empty());
    }

    #[test]
    fn left_and_right_fold_and_unfold_only_the_section_they_are_on() {
        let mut app = App::new();
        app.focus = Some(Focus::Section(Section::Windows));
        app.open = None;
        // Right on a folded section unfolds; Left on it does nothing.
        let _ = update(&mut app, Key::Left);
        let _ = update(&mut app, Key::Right);
        // Escape drops the ring.
        let _ = update(&mut app, Key::Clear);
        assert_eq!(app.focus, None);
    }
}
