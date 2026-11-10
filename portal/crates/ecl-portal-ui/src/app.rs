// SPDX-License-Identifier: AGPL-3.0-only
//! The iced application around a [`Pane`]: messages, the one reply, and the
//! subscriptions. A dialog writes exactly one `PaneReply` and exits; closing
//! the window or pressing Escape is `Cancelled`.

use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Instant;

use ecl_portal_proto::{PaneReply, Requester, ScreenshotMode};
use iced::{event, keyboard, window, Event, Subscription, Task};

use ec_ui::motion::{Animated, Motion};
use ec_ui::tokens::{color, radius};

use crate::model::{Choice, Nav, Opened, Pane, Tab};
use crate::{input, wire};

#[derive(Debug, Clone)]
pub enum Message {
    Key(Nav),
    Tab(Tab),
    Pick(usize),
    Audio(bool),
    Remember(bool),
    /// A click on Deny or Allow (a deliberate pointer press, unlike Enter).
    Choose(Choice),
    Mode(ScreenshotMode),
    Rgba([u8; 4]),
    /// The colour picker's drag ended; nothing to do, the field already follows.
    Released,
    Hex(String),
    Name(String),
    RealName(String),
    Frame(Instant),
    GlassWindow(window::Id),
    GlassReady(bool),
}

pub struct App {
    pub requester: Requester,
    pub pane: Pane,
    /// The requester's icon, when it named an SVG on disk.
    pub icon: Option<PathBuf>,
    /// `None` once the reply is written.
    stream: Option<UnixStream>,
    /// The compositor's blur is behind the window: lay a tint, not a ground.
    pub blur: bool,
    pub radius: f32,
    /// 0 to 1: the content settling in. Snaps with reduce-motion.
    pub enter: Animated<f32>,
}

impl App {
    pub fn new(opened: Opened, stream: UnixStream) -> (Self, Task<Message>) {
        let (rounding, blur) = ec_ui::ipc::fetch_glass();
        ec_ui::ipc::sync_surface_now();
        let motion = fetch_motion();
        let mut enter = Animated::new(0.0, motion);
        enter.set_target(1.0, Instant::now());
        let icon = opened
            .requester
            .icon
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| {
                p.is_absolute() && p.extension().is_some_and(|e| e == "svg") && p.is_file()
            });
        (
            Self {
                requester: opened.requester,
                pane: opened.pane,
                icon,
                stream: Some(stream),
                blur: blur.unwrap_or(false),
                radius: rounding.unwrap_or(radius::WINDOW),
                enter,
            },
            Task::none(),
        )
    }

    pub fn title(&self) -> &'static str {
        self.pane.window().2
    }

    /// Write the reply once and go. A write error is the daemon having gone
    /// away; either way there is nothing more to do.
    fn finish(&mut self, reply: PaneReply) -> Task<Message> {
        if let Some(mut stream) = self.stream.take() {
            let _ = wire::write_reply(&mut stream, &reply);
        }
        iced::exit()
    }
}

/// `animations.events.window-open`, fail-soft. The compositor has already
/// resolved reduce-motion into it, so a snap there is a snap here.
fn fetch_motion() -> Motion {
    ec_ipc::Client::connect()
        .ok()
        .and_then(|mut c| ec_ui::ipc::fetch_animation(&mut c, "window-open"))
        .map_or(Motion::DEFAULT, |a| a.motion)
}

/// The login, the display name and the photo that the account sheet starts
/// from. The only place the system is read; the human can edit all of it.
pub fn system_identity() -> (String, String, Option<String>) {
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).ok();
    let found = uid.and_then(|u| {
        let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
        crate::model::passwd_entry(&passwd, u)
    });
    let (name, real) = found.unwrap_or_else(|| {
        let user = std::env::var("USER").unwrap_or_default();
        (user.clone(), user)
    });
    let image = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".face"))
        .filter(|p| p.is_file())
        .and_then(|p| p.into_os_string().into_string().ok());
    (name, real, image)
}

/// The colour the picker opens on: the style's neutral, so no hue is implied.
pub fn start_color() -> [u8; 3] {
    let [r, g, b, _] = color::neutral().into_rgba8();
    [r, g, b]
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    let reply = match message {
        Message::Key(nav) => app.pane.key(nav),
        Message::Tab(t) => {
            if let Pane::Source(c) = &mut app.pane {
                c.set_tab(t);
            }
            None
        }
        Message::Pick(i) => {
            if let Pane::Source(c) = &mut app.pane {
                c.toggle(i);
            }
            None
        }
        Message::Audio(on) => {
            if let Pane::Source(c) = &mut app.pane {
                c.set_audio(on);
            }
            None
        }
        Message::Remember(on) => {
            match &mut app.pane {
                Pane::Source(c) => c.remember = on,
                Pane::Permission { confirm, .. } | Pane::Email { confirm, .. } => {
                    confirm.set_remember(on)
                }
                _ => {}
            }
            None
        }
        Message::Choose(choice) => match &mut app.pane {
            Pane::Permission { confirm, .. } => {
                confirm.focus = choice;
                match confirm.decide(choice) {
                    crate::model::Verdict::Decide { allow, remember } => {
                        Some(PaneReply::Permission { allow, remember })
                    }
                    _ => None,
                }
            }
            Pane::Email { confirm, .. } => {
                confirm.focus = choice;
                Some(PaneReply::Email {
                    send: choice == Choice::Allow,
                })
            }
            _ => None,
        },
        Message::Mode(m) => {
            if let Pane::Shot(s) = &mut app.pane {
                s.mode = m;
            }
            None
        }
        Message::Rgba(c) => {
            if let Pane::Color(p) = &mut app.pane {
                p.set_rgba(c);
            }
            None
        }
        Message::Released => None,
        Message::Hex(s) => {
            if let Pane::Color(p) = &mut app.pane {
                p.set_draft(s);
            }
            None
        }
        Message::Name(s) => {
            if let Pane::Account { form, .. } = &mut app.pane {
                form.name = s;
            }
            None
        }
        Message::RealName(s) => {
            if let Pane::Account { form, .. } = &mut app.pane {
                form.real_name = s;
            }
            None
        }
        Message::Frame(now) => {
            app.enter.tick(now);
            None
        }
        Message::GlassWindow(id) => {
            return ec_ui::glass::attach_window(id).map(Message::GlassReady)
        }
        // Any update makes iced redraw, which is what lays the glass panes.
        Message::GlassReady(_) => None,
    };
    match reply {
        Some(r) => app.finish(r),
        None => Task::none(),
    }
}

fn on_event(event: Event, status: event::Status, _window: window::Id) -> Option<Message> {
    let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    let nav = input::nav_from(&key, modifiers)?;
    // A focused text field owns typing, arrows and Enter; Escape is always
    // the way out.
    if status == event::Status::Captured && nav != Nav::Escape {
        return None;
    }
    Some(Message::Key(nav))
}

pub fn subscription(app: &App) -> Subscription<Message> {
    let mut subs = vec![
        event::listen_with(on_event),
        // The compositor's close request is a refusal, not a crash.
        window::close_requests().map(|_| Message::Key(Nav::Escape)),
        ec_ui::glass::xdg_window().map(Message::GlassWindow),
    ];
    if app.enter.animating() {
        subs.push(window::frames().map(Message::Frame));
    }
    Subscription::batch(subs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picker_opens_on_a_neutral_grey() {
        let [r, g, b] = start_color();
        assert!(r.abs_diff(g) < 16 && g.abs_diff(b) < 16);
    }

    #[test]
    fn a_text_field_keeps_its_typing_but_not_escape() {
        let key = |k: keyboard::Key| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: k.clone(),
                modified_key: k,
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })
        };
        let id = window::Id::unique();
        let enter = key(keyboard::Key::Named(keyboard::key::Named::Enter));
        let esc = key(keyboard::Key::Named(keyboard::key::Named::Escape));
        assert!(on_event(enter.clone(), event::Status::Captured, id).is_none());
        assert!(matches!(
            on_event(enter, event::Status::Ignored, id),
            Some(Message::Key(Nav::Enter))
        ));
        assert!(matches!(
            on_event(esc, event::Status::Captured, id),
            Some(Message::Key(Nav::Escape))
        ));
    }
}
