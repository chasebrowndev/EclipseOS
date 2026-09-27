// SPDX-License-Identifier: AGPL-3.0-only
//! The iced glue: turns [`Effect`]s into `Task`s and iced events into
//! [`Message`]s. No decisions live here; they are all in [`crate::model`].
//!
//! Anything that can block (`nmcli`, `date`, the helper's disk listing, the
//! control socket) runs on its own thread and comes back as a message, so the
//! window never waits on the machine.

use crate::config::{WriteError, Writer};
use crate::helper::{self, Env, FakeCfg};
use crate::model::{Effect, Inputs, Key, Message, Model, Step};
use crate::net::{self, Link, Snapshot};
use crate::sys;
use eclipse_ui::theme;
use eclipse_welcome::Welcome;
use iced::futures::channel::oneshot;
use iced::futures::StreamExt;
use iced::keyboard::{self, key::Named};
use iced::{event, window, Element, Size, Subscription, Task, Theme};
use serde_json::Value;
use std::sync::mpsc;

/// The application id the compositor's window rules see.
pub const APP_ID: &str = "eclipse-setup";

/// The installer's default UI scale: the token sizes are tuned for a desktop
/// pane, and a full-screen wizard read from arm's length wants more.
pub const DEFAULT_SCALE: f32 = 1.25;

/// What the command line decided.
#[derive(Clone, Debug)]
pub struct Options {
    /// `Some` under `--fake-helper`: everything that reaches outside is simulated.
    pub fake: Option<FakeCfg>,
    pub reduced_motion: bool,
    /// A window of this logical size instead of full screen.
    pub size: Option<(f32, f32)>,
    pub scale: f32,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            fake: None,
            reduced_motion: eclipse_welcome::reduced_motion_from_env(),
            size: None,
            scale: DEFAULT_SCALE,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Welcome(eclipse_welcome::Message),
    Wizard(Message),
}

struct Job {
    key: &'static str,
    value: Value,
    reply: oneshot::Sender<Result<(), WriteError>>,
}

pub struct App {
    model: Model,
    welcome: Welcome,
    env: Env,
    writer: mpsc::Sender<Job>,
}

/// The one thread that owns the control-socket connection, so writes stay in
/// the order they were asked for (layout before variant).
fn spawn_writer(fake: bool) -> mpsc::Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let mut writer = if fake {
            Writer::recording()
        } else {
            Writer::socket()
        };
        for job in rx {
            let result = if crate::config::tolerates_unknown(job.key) {
                writer.set_setup(job.key, job.value)
            } else {
                writer.set(job.key, job.value)
            };
            let _ = job.reply.send(result);
        }
    });
    tx
}

/// The welcome step for these options.
pub fn welcome(version: &str, reduced_motion: bool, scale: f32) -> Welcome {
    Welcome::new(version)
        .reduced_motion(reduced_motion)
        .ui_scale(scale)
}

/// Run `work` on its own thread and map what it returns. `None` means the
/// thread died before answering.
fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    then: impl Fn(Option<T>) -> Message + Send + 'static,
) -> Task<Msg> {
    Task::perform(
        async move {
            let (tx, rx) = oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(work());
            });
            rx.await.ok()
        },
        move |r| Msg::Wizard(then(r)),
    )
}

impl App {
    pub fn boot(options: &Options) -> (App, Task<Msg>) {
        let inputs = Inputs::detect(options.reduced_motion, options.fake.is_some());
        let version = inputs.version.clone();
        let app = App {
            welcome: welcome(&version, options.reduced_motion, options.scale),
            model: Model::new(inputs),
            env: options.fake.clone().map_or_else(Env::real, Env::fake),
            writer: spawn_writer(options.fake.is_some()),
        };
        (app, Welcome::probe().map(Msg::Welcome))
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Welcome(eclipse_welcome::Message::Begin) => self.wizard(Message::Begin),
            Msg::Welcome(m) => self.welcome.update(m).map(Msg::Welcome),
            Msg::Wizard(m) => self.wizard(m),
        }
    }

    fn wizard(&mut self, message: Message) -> Task<Msg> {
        let effects = self.model.update(message);
        Task::batch(effects.into_iter().map(|e| self.run(e)))
    }

    fn run(&self, effect: Effect) -> Task<Msg> {
        match effect {
            Effect::Config { key, value } => {
                let (reply, answer) = oneshot::channel();
                if self.writer.send(Job { key, value, reply }).is_err() {
                    return Task::done(Msg::Wizard(Message::ConfigResult(key, Err(WriteError::NoSocket))));
                }
                Task::perform(
                    async move { answer.await.unwrap_or(Err(WriteError::NoSocket)) },
                    move |r| Msg::Wizard(Message::ConfigResult(key, r)),
                )
            }
            Effect::LoadDisks => {
                let env = self.env.clone();
                off_thread(
                    move || helper::list_disks(&env),
                    |r| {
                        Message::DisksLoaded(
                            r.unwrap_or_else(|| Err("the install helper did not answer".to_owned())),
                        )
                    },
                )
            }
            Effect::ReadClock(zone) => {
                let asked = zone.clone();
                off_thread(
                    move || sys::zone_clock(&zone),
                    move |c| Message::ZoneClock(asked.clone(), c.flatten()),
                )
            }
            Effect::NetScan => {
                let env = self.env.clone();
                off_thread(
                    move || net::scan(&env),
                    |s| {
                        Message::NetScanned(s.unwrap_or(Snapshot {
                            link: Link::NoManager,
                            aps: Vec::new(),
                        }))
                    },
                )
            }
            Effect::NetJoin {
                ssid,
                secure,
                passphrase,
            } => {
                let env = self.env.clone();
                off_thread(
                    move || net::join(&env, &ssid, secure, passphrase),
                    |ok| Message::Joined(ok.unwrap_or(false)),
                )
            }
            Effect::Apply(request) => {
                // The request is consumed here: its password is written to the
                // helper and zeroised on drop.
                let events = helper::apply(&self.env, request);
                Task::stream(events.map(|e| Msg::Wizard(Message::Helper(e))))
            }
            Effect::Reboot => {
                let env = self.env.clone();
                off_thread(
                    move || helper::reboot(&env),
                    |r| Message::Restarted(r.unwrap_or_else(|| Err("the restart did not start".to_owned()))),
                )
            }
            Effect::Focus(id) => iced::widget::operation::focus(id),
            Effect::FocusNext => iced::widget::operation::focus_next(),
            Effect::FocusPrev => iced::widget::operation::focus_previous(),
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        match self.model.step {
            Step::Welcome => self.welcome.view().map(Msg::Welcome),
            _ => crate::view::view(&self.model).map(Msg::Wizard),
        }
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let keys = event::listen_with(key_message);
        if self.model.step == Step::Welcome {
            Subscription::batch([self.welcome.subscription().map(Msg::Welcome), keys])
        } else {
            keys
        }
    }

    pub fn model(&self) -> &Model {
        &self.model
    }
}

/// The wizard's own keys, for events no widget took.
fn key_message(event: iced::Event, status: event::Status, _window: window::Id) -> Option<Msg> {
    if status != event::Status::Ignored {
        return None;
    }
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    let key = match key.as_ref() {
        keyboard::Key::Named(Named::Enter) => Key::Enter,
        keyboard::Key::Named(Named::Escape) => Key::Escape,
        keyboard::Key::Named(Named::ArrowUp) => Key::Up,
        keyboard::Key::Named(Named::ArrowDown) => Key::Down,
        keyboard::Key::Named(Named::Tab) if modifiers.shift() => Key::BackTab,
        keyboard::Key::Named(Named::Tab) => Key::Tab,
        _ => return None,
    };
    Some(Msg::Wizard(Message::Key(key)))
}

pub fn theme_of(_: &App) -> Theme {
    theme::theme()
}

/// Run the wizard.
pub fn run(options: Options) -> iced::Result {
    let settings = match options.size {
        Some((w, h)) => window::Settings {
            size: Size::new(w, h),
            resizable: false,
            decorations: false,
            ..window::Settings::default()
        },
        None => window::Settings {
            fullscreen: true,
            decorations: false,
            ..window::Settings::default()
        },
    };
    let settings = window::Settings {
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.to_owned(),
            ..window::settings::PlatformSpecific::default()
        },
        ..settings
    };
    let scale = options.scale;
    let mut app = iced::application(move || App::boot(&options), App::update, App::view)
        .title("Install EclipseOS")
        .theme(theme_of)
        .subscription(App::subscription)
        .window(settings)
        .scale_factor(move |_| scale)
        .antialiasing(true);
    for font in eclipse_ui::FONTS {
        app = app.font(*font);
    }
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_motion_reaches_the_welcome_step() {
        let calm = welcome("test", true, 1.0);
        assert!(calm.is_ready(), "reduced motion jumps straight to the prompt");
        let full = welcome("test", false, 1.0);
        assert!(!full.is_ready(), "the full animation is not ready at t = 0");
    }

    #[test]
    fn the_wizards_keys_are_the_wizards_only_when_no_widget_took_them() {
        let press = |k| {
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(k),
                modified_key: keyboard::Key::Named(k),
                physical_key: keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::default(),
                text: None,
                repeat: false,
            })
        };
        let id = window::Id::unique();
        assert!(matches!(
            key_message(press(Named::Enter), event::Status::Ignored, id),
            Some(Msg::Wizard(Message::Key(Key::Enter)))
        ));
        assert!(key_message(press(Named::Enter), event::Status::Captured, id).is_none());
        assert!(key_message(press(Named::F5), event::Status::Ignored, id).is_none());
    }
}
