// SPDX-License-Identifier: AGPL-3.0-only
//! The application: state, messages, subscriptions and the window's wiring.
//!
//! Every input the screen takes is a message. A keystroke reaches the secret
//! only while the backend is prompting (or refused and asking again); otherwise
//! it is dropped on the floor. Nothing here logs the text of a key.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ec_greeter::{Settings, Theme as GreeterTheme};
use ec_layout::Layout;
use ec_theme::{derive, Palette, Rgba, Seeds, DARK_SEEDS};
use iced::futures::channel::mpsc::Sender;
use iced::futures::Stream;
use iced::keyboard::key::Named;
use iced::keyboard::{self, Key};
use iced::{event, window, Color, Event, Size, Subscription, Task};

use crate::auth::{Auth, AuthState, FakeAuth, Power};
use crate::backdrop::Backdrop;
use crate::clock::{self, Face, Format};
use crate::glance::{self, Snapshot};
use crate::glass;
use crate::motion::Stage;
use crate::secret::{Secret, Typed};
use crate::tokens::{self, font, to_iced};

/// Where the image shipped with the package lands on the greeter's disk.
pub const SHIPPED_WALLPAPER: &str = "/usr/share/backgrounds/eclipseos/greeter.png";
const DEFAULT_GREETING: &str = "Sign in to continue";
/// The clock face's cadence: once a second is enough for a minute display.
const TICK: Duration = Duration::from_secs(1);

/// Everything the screen can be told.
#[derive(Clone)]
pub enum Msg {
    /// The window's id is known: bind its glass surface.
    Attach(window::Id),
    /// One animation frame.
    Frame(Instant),
    /// The clock's second.
    Tick,
    /// The output changed size.
    Resize(Size),
    /// A Glance poll answered (`None`: the read timed out, keep the last).
    Glance(Option<Snapshot>),
    /// An account row was pressed.
    Pick(String),
    /// Text typed while the field takes keys.
    Typed(Typed),
    Backspace,
    Submit,
    Cancel,
    Power(Power),
}

/// Names only: a message can carry typed text or a user name, and neither is logged.
impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Msg::Attach(_) => "Attach",
            Msg::Frame(_) => "Frame",
            Msg::Tick => "Tick",
            Msg::Resize(_) => "Resize",
            Msg::Glance(_) => "Glance",
            Msg::Pick(_) => "Pick",
            Msg::Typed(_) => "Typed",
            Msg::Backspace => "Backspace",
            Msg::Submit => "Submit",
            Msg::Cancel => "Cancel",
            Msg::Power(_) => "Power",
        })
    }
}

pub struct App {
    pub(crate) pal: Palette,
    pub(crate) stage: Stage,
    pub(crate) auth: Box<dyn Auth>,
    pub(crate) users: Vec<String>,
    pub(crate) selected: Option<String>,
    pub(crate) secret: Secret,
    pub(crate) clock: Face,
    pub(crate) format: Format,
    pub(crate) greeting: String,
    pub(crate) glance: Snapshot,
    /// The wallpaper, if one could be read.
    pub(crate) backdrop: Option<Backdrop>,
    /// Glance data was handed in; do not poll the machine.
    glance_fixed: bool,
}

/// Show the greeter with `settings` and the backend `auth`. Blocks until the
/// window closes.
pub fn run(settings: Settings, auth: Box<dyn Auth>) -> iced::Result {
    run_with(settings, auth, Options::default())
}

/// What a preview changes about [`run`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Where the shipped wallpaper is read from.
    pub shipped_wallpaper: PathBuf,
    /// Fixed Glance data: the sysfs readers are not polled. For screenshots.
    pub glance: Option<Snapshot>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            shipped_wallpaper: PathBuf::from(SHIPPED_WALLPAPER),
            glance: None,
        }
    }
}

/// [`run`] with `options`. The stored wallpaper is chosen by
/// `settings.wallpaper`, the shipped one by `options`.
pub fn run_with(settings: Settings, auth: Box<dyn Auth>, options: Options) -> iced::Result {
    let backdrop = load_backdrop(settings.wallpaper, &options.shipped_wallpaper);
    let mut app0 = App::new(settings, auth);
    app0.backdrop = backdrop;
    if let Some(fixed) = options.glance {
        app0.glance = fixed;
        app0.glance_fixed = true;
    }
    let slot = Mutex::new(Some(app0));
    let boot = move || {
        slot.lock()
            .ok()
            .and_then(|mut g| g.take())
            .unwrap_or_else(App::fallback)
    };
    let mut app = iced::application(boot, App::update, crate::view::view)
        .title("EclipseOS greeter")
        .subscription(App::subscription)
        .style(App::style)
        .transparent(true)
        .window_size(tokens::DEFAULT_WINDOW);
    for face in font::BYTES {
        app = app.font(*face);
    }
    app.run()
}

/// The stored wallpaper when the operator chose one, else the shipped image.
/// A stored image that does not load falls back to the shipped one.
fn load_backdrop(stored: bool, shipped: &Path) -> Option<Backdrop> {
    if stored {
        let path = Path::new(ec_greeter::STATE_DIR).join(ec_greeter::WALLPAPER_FILE);
        if let Some(b) = Backdrop::load(&path) {
            return Some(b);
        }
    }
    Backdrop::load(shipped)
}

/// The layout the screen draws: the operator's when it is usable, else the
/// shipped default. Usable means it validates and holds no session-only tile.
fn usable(l: &Layout) -> bool {
    ec_layout::validate(l).is_ok() && l.tiles.iter().all(|t| !t.kind.needs_session())
}

/// The palette from the operator's theme. Absent, the shipped dark one.
fn seeds(theme: Option<&GreeterTheme>) -> Seeds {
    match theme {
        None => DARK_SEEDS,
        Some(t) => Seeds {
            base: Rgba::rgb(t.base),
            ink: Rgba::rgb(t.ink),
            accent: Rgba::rgb(t.accent),
            accent_text: Some(Rgba::rgb(t.accent_text)),
            ..DARK_SEEDS
        },
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl App {
    fn new(settings: Settings, auth: Box<dyn Auth>) -> Self {
        let layout = settings.layout.filter(usable).unwrap_or_default();
        let greeting = settings
            .greeting
            .map(|g| g.trim().to_owned())
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| DEFAULT_GREETING.to_owned());
        Self::assemble(
            layout,
            settings.theme.as_ref(),
            Format::of(settings.clock),
            greeting,
            auth,
        )
    }

    /// The screen when the settings could not be had at all: shipped layout
    /// and palette, and a backend that grants nothing.
    fn fallback() -> Self {
        Self::assemble(
            Layout::default(),
            None,
            Format::of(None),
            DEFAULT_GREETING.to_owned(),
            Box::new(FakeAuth::new(&[])),
        )
    }

    fn assemble(
        layout: Layout,
        theme: Option<&GreeterTheme>,
        format: Format,
        greeting: String,
        auth: Box<dyn Auth>,
    ) -> Self {
        let now = Instant::now();
        let users = auth.users();
        let mut app = App {
            pal: derive(&seeds(theme)),
            stage: Stage::new(
                layout,
                Size::new(tokens::DEFAULT_WINDOW.0, tokens::DEFAULT_WINDOW.1),
                now,
            ),
            auth,
            users,
            selected: None,
            secret: Secret::new(),
            clock: Face::default(),
            format,
            greeting,
            glance: Snapshot::default(),
            backdrop: None,
            glance_fixed: false,
        };
        app.refresh_clock();
        app
    }

    fn refresh_clock(&mut self) {
        self.clock = clock::face(self.format, now_secs());
    }

    fn state(&self) -> AuthState {
        self.auth.state()
    }

    /// Called every frame. Once the backend is back at rest the screen forgets
    /// the account and the typed text, so nothing lingers on a screen nobody is at.
    fn settle(&mut self) {
        if self.state() == AuthState::Idle {
            self.selected = None;
            if !self.secret.is_empty() {
                self.secret.clear();
            }
        }
    }

    pub(crate) fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Attach(id) => return glass::attach_window(id).discard(),
            Msg::Frame(now) => {
                self.stage.step(now);
                self.settle();
                if self.state() == AuthState::Granted {
                    return iced::exit();
                }
            }
            Msg::Tick => {
                self.refresh_clock();
                if self.state() == AuthState::Granted {
                    return iced::exit();
                }
            }
            Msg::Resize(size) => self.stage.resize(size, Instant::now()),
            Msg::Glance(Some(snapshot)) => self.glance = snapshot,
            Msg::Glance(None) => {}
            Msg::Pick(user) => {
                if matches!(self.state(), AuthState::Idle | AuthState::Refused(_)) {
                    self.selected = Some(user.clone());
                    self.secret.clear();
                    self.auth.begin(&user);
                }
            }
            Msg::Typed(text) => {
                if self.state().takes_keys() {
                    self.secret.push_str(text.as_str());
                }
            }
            Msg::Backspace => {
                if self.state().takes_keys() {
                    self.secret.pop();
                }
            }
            Msg::Submit => {
                if matches!(self.state(), AuthState::Prompting | AuthState::Refused(_))
                    && !self.secret.is_empty()
                {
                    let secret = self.secret.take();
                    self.auth.submit(secret);
                }
            }
            Msg::Cancel => {
                self.secret.clear();
                self.selected = None;
                self.auth.cancel();
            }
            Msg::Power(action) => self.auth.power(action),
        }
        Task::none()
    }

    pub(crate) fn subscription(&self) -> Subscription<Msg> {
        let busy = self.state().busy();
        let mut subs = vec![
            Subscription::run(ticks),
            window::resize_events().map(|(_, size)| Msg::Resize(size)),
            glass::xdg_window().map(Msg::Attach),
            event::listen_with(key),
        ];
        if !self.glance_fixed {
            subs.push(Subscription::run(glance::poll).map(Msg::Glance));
        }
        // Frames are asked for only while something moves or is being waited on.
        if busy || self.stage.animating(Instant::now()) {
            subs.push(window::frames().map(Msg::Frame));
        }
        Subscription::batch(subs)
    }

    pub(crate) fn style(&self, _theme: &iced::Theme) -> iced::theme::Style {
        // Glass: the compositor draws the ground, so the window stays clear.
        // Without it the ground is opaque, drawn here.
        let background = if glass::available() {
            Color::TRANSPARENT
        } else {
            to_iced(self.pal.base)
        };
        iced::theme::Style {
            background_color: background,
            text_color: to_iced(self.pal.text),
        }
    }
}

/// One tick a second, for the clock face.
fn ticks() -> impl Stream<Item = Msg> {
    iced::stream::channel(1, |mut out: Sender<Msg>| async move {
        std::thread::spawn(move || loop {
            if let Err(e) = out.try_send(Msg::Tick) {
                if e.is_disconnected() {
                    break;
                }
            }
            std::thread::sleep(TICK);
        });
        std::future::pending::<()>().await
    })
}

/// Keys to messages. Named keys drive the form; printable text is offered to
/// the field, which takes it only while it is open.
fn key(event: Event, _status: event::Status, _id: window::Id) -> Option<Msg> {
    let Event::Keyboard(keyboard::Event::KeyPressed {
        key, modifiers, text, ..
    }) = event
    else {
        return None;
    };
    match key {
        Key::Named(Named::Enter) => Some(Msg::Submit),
        Key::Named(Named::Backspace) => Some(Msg::Backspace),
        Key::Named(Named::Escape) => Some(Msg::Cancel),
        _ if modifiers.control() || modifiers.alt() || modifiers.logo() => None,
        _ => text.as_deref().and_then(Typed::new).map(Msg::Typed),
    }
}
