// SPDX-License-Identifier: AGPL-3.0-only
//! The Login screen page (ADR 0089): the machine's login screen, changed from
//! Settings.
//!
//! Not schema keys. The current choices are the world-readable
//! `ec_greeter::STATE_DIR/SETTINGS_FILE`; a change is `pkexec
//! /usr/lib/eclipse/ec-greeter-apply apply` with `ec_greeter::frame(..)` on
//! stdin, or `... reset`, run on a worker thread. The compositor draws the
//! password prompt: this page never sees or draws one.
//!
//! The wallpaper is decoded and downscaled here, as the user, to raw RGBA8
//! within `MAX_WIDTH` x `MAX_HEIGHT`. Root only encodes pixels; it never opens
//! the user's file. The path is dropped once converted.

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use ec_greeter::{Clock, Settings, Theme as LoginTheme};
use iced::widget::{button, column, container, row, stack, text, text_input, Space};
use iced::{Alignment, Element, Length, Task, Theme};

use ec_ui::theme;
use ec_ui::tokens::{color, font, radius, size, space};
use ec_ui::widget::{
    edge_note, glass_inset as inset, glass_panel as panel, inset_list, micro_label, pick_pill, pill,
    pill_group, Toggle,
};

use crate::app::{App, Message};
use crate::conn::Problem;

/// The root helper, fixed. No argument is ever built from user input.
pub const HELPER: &str = "/usr/lib/eclipse/ec-greeter-apply";
/// Refuse to read more than this of a chosen file.
const MAX_FILE: u64 = 128 * 1024 * 1024;
/// The preview thumbnail fits this.
const PREVIEW: (u32, u32) = (640, 360);
/// The shipped wallpaper, as `packaging/greetd/regreet.toml` names it.
const SHIPPED_WALLPAPER: &str = "/usr/share/backgrounds/eclipseos/greeter.png";

/// A converted wallpaper, ready to send.
#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, straight RGBA8.
    pub rgba: Arc<Vec<u8>>,
    /// A small copy for the preview (`iced::widget::image(handle)`).
    pub preview: iced::widget::image::Handle,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({}x{})", self.width, self.height)
    }
}

/// What the page will do with the wallpaper.
#[derive(Debug, Clone, Default)]
pub enum Wallpaper {
    /// Leave whatever the machine has now (the shipped image unless the
    /// current settings say otherwise).
    #[default]
    Keep,
    /// Back to the shipped image.
    Shipped,
    /// A newly chosen image.
    New(Image),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Apply,
    Reset,
}

/// How the helper's run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// pkexec 126: the person dismissed the prompt.
    Dismissed,
    /// pkexec 127: not authorised.
    NotAuthorised,
    /// 2: the helper refused the input.
    Invalid,
    /// Anything else, including a signal or pkexec missing.
    Failed(String),
}

impl Outcome {
    /// Exit code to outcome. `None` is a process killed by a signal.
    pub fn from_exit(code: Option<i32>) -> Outcome {
        match code {
            Some(0) => Outcome::Done,
            Some(126) => Outcome::Dismissed,
            Some(127) => Outcome::NotAuthorised,
            Some(2) => Outcome::Invalid,
            Some(n) => Outcome::Failed(format!("the helper exited with status {n}")),
            None => Outcome::Failed("the helper was killed".into()),
        }
    }

    /// The sentence for the banner.
    pub fn message(&self, action: Action) -> String {
        match (self, action) {
            (Outcome::Done, Action::Apply) => "Login screen updated.".into(),
            (Outcome::Done, Action::Reset) => "Login screen back to the default.".into(),
            (Outcome::Dismissed, _) => "Cancelled. Nothing was changed.".into(),
            (Outcome::NotAuthorised, _) => "Not authorised. Nothing was changed.".into(),
            (Outcome::Invalid, _) => "The login screen settings were refused as invalid.".into(),
            (Outcome::Failed(why), _) => format!("Could not change the login screen: {why}."),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Outcome::Done)
    }
}

/// Runs the helper: argv after `pkexec`, then stdin. The exit code, `None` for
/// a signal. Injected so tests never call pkexec.
pub type Runner = dyn Fn(&[&str], &[u8]) -> std::io::Result<Option<i32>> + Send + Sync;

/// The real runner: `pkexec <argv>`.
pub fn pkexec(argv: &[&str], stdin: &[u8]) -> std::io::Result<Option<i32>> {
    let mut child = Command::new("pkexec")
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut pipe) = child.stdin.take() {
        // The helper may exit before reading it all; its status says why.
        let _ = pipe.write_all(stdin);
    }
    Ok(child.wait()?.code())
}

/// Run one action through `run`.
pub fn run_action(run: &Runner, action: Action, frame: &[u8]) -> Outcome {
    let result = match action {
        Action::Apply => run(&[HELPER, "apply"], frame),
        Action::Reset => run(&[HELPER, "reset"], &[]),
    };
    match result {
        Ok(code) => Outcome::from_exit(code),
        Err(e) => Outcome::Failed(format!("could not start pkexec: {e}")),
    }
}

/// Read, decode and downscale a chosen file. As the user, off the UI thread.
pub fn convert_file(path: &Path) -> Result<Image, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("could not open the image: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not read the image: {e}"))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("that image file is too large".into());
    }
    convert_bytes(&bytes)
}

/// Decode `bytes`, fit them in `MAX_WIDTH` x `MAX_HEIGHT` keeping aspect (never
/// enlarging), and convert to RGBA8. Bad or oversized input is an `Err`.
pub fn convert_bytes(bytes: &[u8]) -> Result<Image, String> {
    let bad = |e: image::ImageError| format!("could not read that image: {e}");
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("could not read that image: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(1 << 30);
    reader.limits(limits);
    let mut img = reader.decode().map_err(bad)?;
    if img.width() > ec_greeter::MAX_WIDTH || img.height() > ec_greeter::MAX_HEIGHT {
        img = img.resize(
            ec_greeter::MAX_WIDTH,
            ec_greeter::MAX_HEIGHT,
            image::imageops::FilterType::Triangle,
        );
    }
    let thumb = img.thumbnail(PREVIEW.0, PREVIEW.1).to_rgba8();
    let preview = iced::widget::image::Handle::from_rgba(thumb.width(), thumb.height(), thumb.into_raw());
    let rgba = img.to_rgba8();
    Ok(Image {
        width: rgba.width(),
        height: rgba.height(),
        rgba: Arc::new(rgba.into_raw()),
        preview,
    })
}

/// The active theme as the login screen takes it, if it passes the contrast
/// check. `accent_text` falls back to the accent, as the palette does.
pub fn login_theme(seeds: &ec_theme::Seeds) -> Option<LoginTheme> {
    ec_theme::validate(seeds).ok()?;
    let pack = |c: ec_theme::Rgba| -> u32 {
        let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        (ch(c.r()) << 16) | (ch(c.g()) << 8) | ch(c.b())
    };
    Some(LoginTheme {
        base: pack(seeds.base),
        ink: pack(seeds.ink),
        accent: pack(seeds.accent),
        accent_text: pack(seeds.accent_text.unwrap_or(seeds.accent)),
    })
}

/// A greeting as typed or pasted: a run of control whitespace (newline, tab)
/// becomes one space, never a leading one or a double; other control
/// characters are dropped; at most `MAX_GREETING` characters.
pub fn clean_greeting(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_control() {
            if c.is_whitespace() && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out.chars().take(ec_greeter::MAX_GREETING).collect()
}

/// The pictures the preview shows for wallpapers it did not just convert.
#[derive(Debug, Clone, Default)]
pub struct Pictures {
    pub stored: Option<iced::widget::image::Handle>,
    pub shipped: Option<iced::widget::image::Handle>,
}

/// Decode a world-readable wallpaper for the preview. Any failure is `None`
/// (the preview falls back to flat base; a dev tree may lack the file).
fn picture(path: &Path) -> Option<iced::widget::image::Handle> {
    convert_file(path).ok().map(|i| i.preview)
}

/// The page's state.
#[derive(Debug, Default)]
pub struct LoginScreen {
    /// What the machine has now. `None` is the shipped look (file absent or
    /// unreadable).
    pub current: Option<Settings>,
    /// The active theme's colours, if they pass the contrast check. `None`
    /// means "Use my theme" is unavailable.
    pub theme: Option<LoginTheme>,
    pub wallpaper: Wallpaper,
    /// "Use my theme" toggle.
    pub use_theme: bool,
    /// The greeting field; empty means the shipped greeting.
    pub greeting: String,
    /// `None` leaves the shipped clock.
    pub clock: Option<Clock>,
    /// The path field, for choosing an image without the portal.
    pub path: String,
    /// An image is being decoded.
    pub converting: bool,
    /// The helper is running (the compositor's password prompt may be up).
    pub applying: bool,
    /// The last result, for the banner (`Outcome::message`).
    pub last: Option<(Action, Outcome)>,
    /// Why Browse or the image failed.
    pub image_error: Option<String>,
    /// Bumped per conversion so a stale result is ignored.
    generation: u64,
    /// Preview picture of the stored wallpaper (display only).
    stored: Option<iced::widget::image::Handle>,
    /// Preview picture of the shipped wallpaper (display only).
    shipped: Option<iced::widget::image::Handle>,
    /// Bumped per picture refresh so a late decode cannot overwrite a newer one.
    picture_gen: u64,
}

impl LoginScreen {
    /// The settings Apply would send.
    pub fn pending(&self) -> Settings {
        let wallpaper = match &self.wallpaper {
            Wallpaper::Keep => self.current.as_ref().is_some_and(|s| s.wallpaper),
            Wallpaper::Shipped => false,
            Wallpaper::New(_) => true,
        };
        let greeting = clean_greeting(&self.greeting);
        Settings {
            wallpaper,
            theme: if self.use_theme { self.theme } else { None },
            greeting: (!greeting.trim().is_empty()).then_some(greeting),
            clock: self.clock,
        }
    }

    /// The bytes for the helper's stdin.
    pub fn frame(&self) -> Vec<u8> {
        let image = match &self.wallpaper {
            Wallpaper::New(i) => Some((i.width, i.height, i.rgba.as_slice())),
            _ => None,
        };
        ec_greeter::frame(&self.pending(), image)
    }

    /// Whether Apply can be offered: only with something to send, so a no-op
    /// never reaches pkexec.
    pub fn can_apply(&self) -> bool {
        !self.applying && !self.converting && (!self.use_theme || self.theme.is_some()) && dirty(self)
    }

    /// Whether Reset can be offered: only when the machine has something
    /// stored. Reset runs the root helper; it does not clear unsent edits
    /// (Reload does), so edits alone never enable it.
    pub fn can_reset(&self) -> bool {
        !self.applying && self.current.clone().unwrap_or_default() != Settings::default()
    }

    /// The picture behind the preview, if one is decoded.
    fn backdrop(&self) -> Option<&iced::widget::image::Handle> {
        match &self.wallpaper {
            Wallpaper::New(i) => Some(&i.preview),
            Wallpaper::Shipped => self.shipped.as_ref(),
            Wallpaper::Keep if self.current.as_ref().is_some_and(|s| s.wallpaper) => self.stored.as_ref(),
            Wallpaper::Keep => self.shipped.as_ref(),
        }
    }
    /// Set the controls from `s`.
    fn adopt(&mut self, s: Option<Settings>) {
        let s = s.unwrap_or_default();
        self.use_theme = s.theme.is_some();
        self.greeting = s.greeting.clone().unwrap_or_default();
        self.clock = s.clock;
        self.wallpaper = Wallpaper::Keep;
        self.current = Some(s);
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// Browse...: the portal. May fail on a machine with no file chooser; the
    /// error lands in `image_error` and the path field still works.
    Browse,
    Browsed(Result<Option<PathBuf>, String>),
    /// Typing in the path field.
    PathEdited(String),
    /// Enter in the path field (or a "Use" button): convert that file.
    PathSubmit,
    /// Conversion finished (internal; carries the generation).
    Converted(u64, Result<Image, String>),
    /// The preview pictures decoded (internal; carries the picture generation).
    Pictures(u64, Pictures),
    /// "Revert to shipped wallpaper".
    UseShippedWallpaper,
    /// "Use my theme" toggle.
    UseTheme(bool),
    /// The greeting field.
    Greeting(String),
    /// A clock preset, or `None` for the shipped default.
    Clock(Option<Clock>),
    /// Apply (asks for authorisation via the compositor prompt).
    Apply,
    /// Reset to default.
    Reset,
    /// The helper finished (internal).
    Done(Action, Outcome),
    /// Close the banner.
    Dismiss,
}

fn msg(m: Msg) -> Message {
    Message::LoginScreen(m)
}

/// Read the machine's current settings and the active theme. Call when the
/// page opens. Discards unsent edits.
pub fn load(app: &mut App) {
    let current = std::fs::read_to_string(Path::new(ec_greeter::STATE_DIR).join(ec_greeter::SETTINGS_FILE))
        .ok()
        .and_then(|t| Settings::parse(&t).ok());
    let theme = app
        .conn
        .call("get_theme", serde_json::json!({}))
        .ok()
        .and_then(|v| ec_ui::ipc::parse_theme(&v))
        .and_then(|info| login_theme(&info.seeds));
    let p = &mut app.login;
    p.theme = theme;
    p.adopt(current);
    // A saved theme the active one no longer matches still shows as on, with a
    // live toggle: the person can always turn it off (see `theme_row`).
    p.last = None;
    p.image_error = None;
    p.path.clear();
}

fn off_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
    to: impl Fn(T) -> Msg + Send + 'static,
) -> Task<Message> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    Task::perform(async move { rx.await.ok() }, move |r| match r {
        Some(v) => msg(to(v)),
        None => msg(Msg::Dismiss),
    })
}

/// `load`, then decode the preview pictures off the UI thread.
pub fn reload(app: &mut App) -> Task<Message> {
    load(app);
    refresh_pictures(&mut app.login)
}

/// Decode the stored and (once) the shipped wallpaper for the preview.
pub fn refresh_pictures(p: &mut LoginScreen) -> Task<Message> {
    // A debug preview that pinned its own pictures keeps them.
    #[cfg(debug_assertions)]
    if std::env::var("SETTINGS_PREVIEW_LOGIN").is_ok_and(|v| v.contains("stored:") || v.contains("shipped:"))
    {
        return Task::none();
    }
    p.picture_gen += 1;
    let generation = p.picture_gen;
    let want_stored = p.current.as_ref().is_some_and(|s| s.wallpaper);
    let want_shipped = p.shipped.is_none();
    off_thread(
        move || Pictures {
            stored: want_stored
                .then(|| picture(&Path::new(ec_greeter::STATE_DIR).join(ec_greeter::WALLPAPER_FILE)))
                .flatten(),
            shipped: want_shipped
                .then(|| picture(Path::new(SHIPPED_WALLPAPER)))
                .flatten(),
        },
        move |pics| Msg::Pictures(generation, pics),
    )
}

fn convert(p: &mut LoginScreen, path: PathBuf) -> Task<Message> {
    p.generation += 1;
    p.converting = true;
    p.image_error = None;
    let generation = p.generation;
    off_thread(
        move || convert_file(&path),
        move |r| Msg::Converted(generation, r),
    )
}

fn run(p: &mut LoginScreen, action: Action, runner: Arc<Runner>) -> Task<Message> {
    if p.applying
        || (action == Action::Apply && !p.can_apply())
        || (action == Action::Reset && !p.can_reset())
    {
        return Task::none();
    }
    p.applying = true;
    p.last = None;
    let frame = if action == Action::Apply {
        p.frame()
    } else {
        Vec::new()
    };
    off_thread(
        move || run_action(&*runner, action, &frame),
        move |o| Msg::Done(action, o),
    )
}

pub fn update(app: &mut App, m: Msg) -> Task<Message> {
    let p = &mut app.login;
    match m {
        Msg::Browse => {
            return Task::perform(crate::portal::pick_image(), |r| msg(Msg::Browsed(r)));
        }
        Msg::Browsed(Ok(Some(path))) => return convert(p, path),
        Msg::Browsed(Ok(None)) => {}
        Msg::Browsed(Err(e)) => {
            // The detail (a bus error) goes to the log; the page says one thing.
            eprintln!("ec-settings: file chooser: {e}");
            p.image_error = Some("No file chooser is available. Type the image's path instead.".into());
        }
        Msg::PathEdited(s) => p.path = s,
        Msg::PathSubmit => {
            let t = p.path.trim();
            if !t.is_empty() {
                let path = PathBuf::from(t);
                return convert(p, path);
            }
        }
        Msg::Converted(g, r) => {
            if g == p.generation {
                p.converting = false;
                match r {
                    Ok(img) => {
                        p.wallpaper = Wallpaper::New(img);
                        p.path.clear();
                    }
                    Err(e) => p.image_error = Some(e),
                }
            }
        }
        Msg::Pictures(g, pics) => {
            if g == p.picture_gen {
                p.stored = pics.stored;
                if pics.shipped.is_some() {
                    p.shipped = pics.shipped;
                }
            }
        }
        Msg::UseShippedWallpaper => {
            p.generation += 1;
            p.converting = false;
            p.path.clear();
            p.wallpaper = Wallpaper::Shipped;
            p.image_error = None;
        }
        Msg::UseTheme(on) => p.use_theme = on && p.theme.is_some(),
        Msg::Greeting(s) => p.greeting = clean_greeting(&s),
        Msg::Clock(c) => p.clock = c,
        Msg::Apply => return run(p, Action::Apply, Arc::new(pkexec)),
        Msg::Reset => return run(p, Action::Reset, Arc::new(pkexec)),
        Msg::Done(action, outcome) => {
            p.applying = false;
            let mut task = Task::none();
            if outcome.is_success() {
                // Keep drawing the image just sent until the stored one is read.
                if let Wallpaper::New(i) = &p.wallpaper {
                    p.stored = Some(i.preview.clone());
                }
                task = reload(app);
            }
            app.login.last = Some((action, outcome));
            return task;
        }
        Msg::Dismiss => app.login.last = None,
    }
    Task::none()
}

/// A banner problem for a failed run, for views that reuse the app banner.
pub fn problem(action: Action, outcome: &Outcome) -> Option<Problem> {
    (!outcome.is_success()).then(|| Problem::Other(outcome.message(action)))
}

// ---- the view ----------------------------------------------------------

/// What the shipped greeter says and shows when nothing is set.
const SHIPPED_GREETING: &str =
    "ECLIPSE OS  \u{b7}  ABYSS COMPOSITOR  \u{b7}  SELECT A SESSION AND AUTHENTICATE";
/// The greeter's own clock when `[widget.clock]` is absent.
const SHIPPED_CLOCK: &str = "Tue 09:41";

fn clock_sample(c: Option<Clock>) -> &'static str {
    match c {
        None => SHIPPED_CLOCK,
        Some(Clock::Time24) => "09:41",
        Some(Clock::Time12) => "09:41 AM",
        Some(Clock::Date24) => "Tue 04 Nov  09:41",
        Some(Clock::Date12) => "Tue 04 Nov  09:41 AM",
    }
}

fn clock_label(c: Option<Clock>) -> &'static str {
    match c {
        None => "Shipped default",
        Some(Clock::Time24) => "24-hour",
        Some(Clock::Time12) => "12-hour",
        Some(Clock::Date24) => "24-hour with date",
        Some(Clock::Date12) => "12-hour with date",
    }
}

/// Unsent edits: anything Apply would change on the machine.
fn dirty(p: &LoginScreen) -> bool {
    matches!(p.wallpaper, Wallpaper::New(_)) || p.pending() != p.current.clone().unwrap_or_default()
}

/// The header's two-line chip: what the machine has now.
pub fn status(app: &App) -> (String, String) {
    let p = &app.login;
    let s = p.current.clone().unwrap_or_default();
    let changed = [
        s.wallpaper,
        s.theme.is_some(),
        s.greeting.is_some(),
        s.clock.is_some(),
    ]
    .into_iter()
    .filter(|c| *c)
    .count();
    if changed == 0 {
        ("shipped look".into(), "nothing customised".into())
    } else {
        ("customised".into(), format!("{changed} of 4 changed"))
    }
}

fn msg_text<'a>(t: impl Into<String>, size: f32) -> iced::widget::Text<'a, Theme> {
    text(t.into()).font(font::UI).size(size)
}

fn body<'a>(t: impl Into<String>) -> iced::widget::Text<'a, Theme> {
    msg_text(t, size::BODY)
}

fn small<'a>(t: impl Into<String>) -> iced::widget::Text<'a, Theme> {
    msg_text(t, size::BODY_SMALL)
}

fn mono_tertiary<'a>(t: impl Into<String>) -> iced::widget::Text<'a, Theme> {
    text(t.into())
        .font(font::DATA)
        .size(size::MICRO)
        .style(theme::text_tertiary)
}

fn padded<'a>(e: impl Into<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    container(e)
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .into()
}

/// The login screen as it would look after Apply: the picture, the clock at
/// the top, the glass card, the power pills. Drawn from `pending()`, so the
/// colours, greeting and clock are the ones that would be sent. The clock and
/// pills sit straight on the picture, as on the real screen.
fn preview(p: &LoginScreen) -> Element<'_, Message, Theme> {
    use ec_ui::tokens::login_preview as lp;
    let pending = p.pending();
    let seeds = ec_theme::DARK_SEEDS;
    let (base, ink, accent) = match pending.theme {
        Some(t) => (color::data(t.base), color::data(t.ink), color::data(t.accent)),
        None => (
            color::to_iced(seeds.base),
            color::to_iced(seeds.ink),
            color::to_iced(seeds.accent),
        ),
    };
    let fixed = (Length::Fixed(lp::W), Length::Fixed(lp::H));

    let ground: Element<'_, Message, Theme> = match p.backdrop() {
        Some(handle) => iced::widget::image(handle.clone())
            .width(fixed.0)
            .height(fixed.1)
            .content_fit(iced::ContentFit::Cover)
            .border_radius(lp::RADIUS)
            .into(),
        None => container(Space::new())
            .width(fixed.0)
            .height(fixed.1)
            .style(move |_t: &Theme| container::Style {
                background: Some(base.into()),
                border: iced::Border {
                    radius: lp::RADIUS.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into(),
    };

    let clock = container(
        text(clock_sample(pending.clock))
            .font(font::DATA_MEDIUM)
            .size(lp::CLOCK_SIZE)
            .color(ink),
    )
    .padding([lp::EDGE, 0.0])
    .width(fixed.0)
    .align_x(Alignment::Center);

    // Over a chosen picture the real card is dense base, not light glass.
    let card_fill = if pending.wallpaper {
        base.scale_alpha(lp::CARD_DENSE)
    } else {
        ink.scale_alpha(lp::CARD_FILL)
    };
    let greeting = pending
        .greeting
        .clone()
        .unwrap_or_else(|| SHIPPED_GREETING.to_owned());
    // The real card grows to fit the greeting on one line; so does this one.
    // The spacer is the card's minimum inner width.
    let card = container(
        column![
            Space::new()
                .width(Length::Fixed(lp::CARD_W - 2.0 * lp::CARD_PAD))
                .height(0),
            text(greeting)
                .font(font::UI_MEDIUM)
                .size(lp::GREETING_SIZE)
                .color(ink)
                .wrapping(iced::widget::text::Wrapping::None),
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fixed(lp::FIELD_H))
                .style(move |_t: &Theme| container::Style {
                    background: Some(ink.scale_alpha(lp::FIELD_FILL).into()),
                    border: iced::Border {
                        radius: lp::BUTTON_RADIUS.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            container(
                text("Login")
                    .font(font::UI_MEDIUM)
                    .size(lp::BUTTON_SIZE)
                    .color(base)
            )
            .width(Length::Fill)
            .height(Length::Fixed(lp::BUTTON_H))
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .style(move |_t: &Theme| container::Style {
                background: Some(accent.into()),
                border: iced::Border {
                    radius: lp::BUTTON_RADIUS.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        ]
        .spacing(lp::CARD_GAP)
        .align_x(Alignment::Center),
    )
    .padding(lp::CARD_PAD)
    .width(Length::Shrink)
    .style(move |_t: &Theme| container::Style {
        background: Some(card_fill.into()),
        border: iced::Border {
            color: ink.scale_alpha(lp::CARD_RIM),
            width: space::HAIRLINE,
            radius: lp::CARD_RADIUS.into(),
        },
        ..Default::default()
    });
    let middle = container(card)
        .width(fixed.0)
        .height(fixed.1)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);

    let power = |label: &'static str| {
        container(text(label).font(font::UI_MEDIUM).size(lp::PILL_SIZE).color(ink))
            .padding([lp::PILL_PAD_Y, lp::PILL_PAD_X])
            .style(move |_t: &Theme| container::Style {
                background: Some(ink.scale_alpha(lp::PILL_FILL).into()),
                border: iced::Border {
                    color: ink.scale_alpha(lp::PILL_RIM),
                    width: space::HAIRLINE,
                    radius: radius::PILL.into(),
                },
                ..Default::default()
            })
    };
    let pills = container(row![power("Reboot"), power("Power Off")].spacing(lp::PILL_GAP))
        .padding([lp::EDGE, 0.0])
        .width(fixed.0)
        .height(fixed.1)
        .align_x(Alignment::Center)
        .align_y(Alignment::End);

    stack![ground, clock, middle, pills]
        .width(fixed.0)
        .height(fixed.1)
        .into()
}

/// What picture the preview is standing in for.
fn picture_caption(p: &LoginScreen) -> String {
    match &p.wallpaper {
        Wallpaper::New(i) => format!("new image \u{b7} {}\u{d7}{}", i.width, i.height),
        Wallpaper::Shipped => "shipped wallpaper".into(),
        Wallpaper::Keep if p.current.as_ref().is_some_and(|s| s.wallpaper) => "current wallpaper".into(),
        Wallpaper::Keep => "shipped wallpaper".into(),
    }
}

/// The hero: the login screen itself, at its proportions, centred in a glass
/// panel. A canvas, not a list.
fn hero(app: &App) -> Element<'_, Message, Theme> {
    let p = &app.login;
    panel(
        app.glass_radius,
        app.hairline_cards,
        column![
            row![
                micro_label("login screen"),
                Space::new().width(Length::Fill),
                mono_tertiary(picture_caption(p)),
            ]
            .align_y(Alignment::Center),
            container(preview(p))
                .width(Length::Fill)
                .align_x(Alignment::Center),
            small("Drawn from what Apply will send. The clock and buttons sit straight on the picture.")
                .style(theme::text_tertiary),
        ]
        .spacing(space::ROW_Y),
    )
}

/// The wallpaper row: Browse, the path, Use, and back to the shipped image.
fn wallpaper_row(p: &LoginScreen) -> Element<'_, Message, Theme> {
    let busy = p.converting;
    let state = if busy {
        "Reading the image\u{2026}".to_owned()
    } else {
        match &p.wallpaper {
            Wallpaper::New(i) => format!("{}\u{d7}{} ready", i.width, i.height),
            Wallpaper::Shipped => "shipped".to_owned(),
            Wallpaper::Keep if p.current.as_ref().is_some_and(|s| s.wallpaper) => "current".to_owned(),
            Wallpaper::Keep => "shipped".to_owned(),
        }
    };
    let path = text_input("/home/you/Pictures/photo.png", &p.path)
        .on_input(|s| msg(Msg::PathEdited(s)))
        .on_submit(msg(Msg::PathSubmit))
        .font(font::DATA)
        .size(size::BODY)
        .width(Length::Fill)
        .style(theme::eclipse_input);
    let can_use = !busy && !p.path.trim().is_empty();
    let mut col = column![
        row![
            body("Wallpaper").style(theme::text_primary),
            Space::new().width(Length::Fill),
            mono_tertiary(state),
        ]
        .align_y(Alignment::Center),
        row![
            pick_pill("Browse\u{2026}", false, (!busy).then(|| msg(Msg::Browse))),
            path,
            pick_pill("Use", false, can_use.then(|| msg(Msg::PathSubmit))),
        ]
        .spacing(space::PILL_GAP)
        .align_y(Alignment::Center),
        row![
            pick_pill(
                "Use shipped wallpaper",
                false,
                Some(msg(Msg::UseShippedWallpaper))
            ),
            small("PNG, JPEG or WebP. It is shrunk to fit 3840\u{d7}2160.").style(theme::text_tertiary),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center),
    ]
    .spacing(space::ROW_Y);
    if let Some(e) = &p.image_error {
        col = col.push(small(e.clone()).style(theme::text_danger));
    }
    padded(col)
}

/// A label and its hint at the left, a control at the right.
fn setting_row<'a>(
    label: &str,
    hint: String,
    hint_style: fn(&Theme) -> iced::widget::text::Style,
    control: Element<'a, Message, Theme>,
) -> Element<'a, Message, Theme> {
    padded(
        row![
            column![
                body(label.to_owned()).style(theme::text_primary),
                small(hint).style(hint_style)
            ]
            .spacing(space::LINE_GAP)
            .width(Length::Fill),
            control,
        ]
        .spacing(space::BLOCK)
        .align_y(Alignment::Center),
    )
}

fn theme_row(p: &LoginScreen) -> Element<'_, Message, Theme> {
    match &p.theme {
        None if p.use_theme => setting_row(
            "Use my theme",
            "On from a saved choice, but the active theme's text would be too faint to read on the login screen. \
             Turn it off to apply other changes."
                .into(),
            theme::text_tertiary,
            Toggle::new(true, |_| msg(Msg::UseTheme(false))).into(),
        ),
        Some(_) => setting_row(
            "Use my theme",
            "Colours of the login screen follow the active theme.".into(),
            theme::text_tertiary,
            Toggle::new(p.use_theme, |on| msg(Msg::UseTheme(on))).into(),
        ),
        None => setting_row(
            "Use my theme",
            "Unavailable: the active theme's text would be too faint to read on the login screen.".into(),
            theme::text_tertiary,
            Toggle::locked(false).into(),
        ),
    }
}

fn greeting_row(p: &LoginScreen) -> Element<'_, Message, Theme> {
    let n = p.greeting.chars().count();
    let field = text_input("Empty uses the shipped greeting", &p.greeting)
        .on_input(|s| msg(Msg::Greeting(s)))
        .font(font::UI)
        .size(size::BODY)
        .width(Length::Fill)
        .style(theme::eclipse_input);
    padded(
        column![
            row![
                body("Greeting").style(theme::text_primary),
                Space::new().width(Length::Fill),
                text(format!("{n}/{}", ec_greeter::MAX_GREETING))
                    .font(font::DATA)
                    .size(size::MICRO)
                    .style(if n >= ec_greeter::MAX_GREETING {
                        theme::text_secondary
                    } else {
                        theme::text_tertiary
                    }),
            ]
            .align_y(Alignment::Center),
            field,
        ]
        .spacing(space::ROW_Y),
    )
}

fn clock_row(p: &LoginScreen) -> Element<'_, Message, Theme> {
    let mut pills = vec![pick_pill(
        clock_label(None),
        p.clock.is_none(),
        Some(msg(Msg::Clock(None))),
    )];
    pills.extend(Clock::ALL.into_iter().map(|c| {
        pick_pill(
            clock_label(Some(c)),
            p.clock == Some(c),
            Some(msg(Msg::Clock(Some(c)))),
        )
    }));
    padded(
        column![
            row![
                body("Clock").style(theme::text_primary),
                Space::new().width(Length::Fill),
                mono_tertiary(clock_sample(p.clock)),
            ]
            .align_y(Alignment::Center),
            pill_group(pills),
        ]
        .spacing(space::ROW_Y),
    )
}

fn settings(p: &LoginScreen) -> Element<'_, Message, Theme> {
    inset_list(
        "appearance",
        vec![wallpaper_row(p), theme_row(p), greeting_row(p), clock_row(p)],
    )
}

/// Apply, the quiet Reset, and the one line about the password prompt.
fn actions(p: &LoginScreen) -> Element<'_, Message, Theme> {
    let apply: Element<'_, Message, Theme> = if p.applying {
        pick_pill("Waiting for authorisation\u{2026}", false, None)
    } else if p.can_apply() {
        // Lifted white while there is something to send; never yellow.
        pick_pill("Apply", dirty(p), Some(msg(Msg::Apply)))
    } else {
        pick_pill("Apply", false, None)
    };
    let reset = button(small("Reset to default").font(font::UI_MEDIUM))
        .padding([space::PILL_Y, space::PILL_X])
        .on_press_maybe(p.can_reset().then(|| msg(Msg::Reset)))
        .style(theme::link);
    let mut col = column![row![apply, reset]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center),]
    .spacing(space::ROW_Y);
    if let Some(banner) = result_banner(p) {
        col = col.push(banner);
    }
    col.push(
        small(
            "Apply asks for an administrator password in a system prompt drawn by the compositor, \
             not here. It changes the login screen for every user of this computer.",
        )
        .style(theme::text_tertiary),
    )
    .into()
}

fn result_banner(p: &LoginScreen) -> Option<Element<'_, Message, Theme>> {
    let (action, outcome) = p.last.as_ref()?;
    let (head, edge) = if outcome.is_success() {
        ("Done", color::ok())
    } else {
        ("Nothing changed", color::danger())
    };
    Some(inset(
        row![
            edge_note(head, &outcome.message(*action), edge),
            container(pill("Dismiss", false, msg(Msg::Dismiss))).padding([space::ROW_Y, space::CARD]),
        ]
        .align_y(Alignment::Center),
    ))
}

/// Every block after the header.
///
/// Shape: the preview as a spatial canvas (glass panel) -> the settings as one
/// inset list of hairlined rows -> the unboxed Apply line, with the result
/// banner (inset, only after a run) directly under the buttons so it shows
/// where the person just clicked and nothing above it moves. No two adjacent
/// blocks share a silhouette.
///
/// Accent ledger: one live yellow, the "Use my theme" toggle while it is on:
/// what the login screen follows now. Nothing else in the pane is accented;
/// the choices and Apply lift white (`pick_pill`). The preview's Login button
/// wears the *previewed* theme's accent: that is the other screen's colour,
/// not this pane's.
pub fn blocks(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let p = &app.login;
    vec![hero(app), settings(p), actions(p)]
}

/// Debug builds only: `SETTINGS_PREVIEW_LOGIN=<flag>,..` opens the page in a
/// state, with nothing run, so each can be screenshotted. Flags: `image:<path>`,
/// `theme` (use the dark seeds), `notheme` (the active theme is unusable),
/// `greeting:<text>`, `long` (an 80-character greeting), `clock:<0-3>`,
/// `ok`, `fail`, `error`, `busy`.
#[cfg(debug_assertions)]
pub fn preview_env(p: &mut LoginScreen) {
    let Ok(spec) = std::env::var("SETTINGS_PREVIEW_LOGIN") else {
        return;
    };
    for flag in spec.split(',') {
        let (key, val) = flag.split_once(':').unwrap_or((flag, ""));
        match key {
            "image" => {
                if let Ok(i) = convert_file(Path::new(val)) {
                    p.wallpaper = Wallpaper::New(i);
                }
            }
            "theme" => {
                p.theme = login_theme(&ec_theme::DARK_SEEDS);
                p.use_theme = true;
            }
            "notheme" => {
                p.theme = None;
                p.use_theme = false;
            }
            "greeting" => p.greeting = clean_greeting(val),
            "long" => p.greeting = clean_greeting(&"Welcome back, good to see you. ".repeat(5)),
            "clock" => p.clock = val.parse::<usize>().ok().and_then(|n| Clock::ALL.get(n).copied()),
            "ok" => p.last = Some((Action::Apply, Outcome::Done)),
            "fail" => p.last = Some((Action::Apply, Outcome::NotAuthorised)),
            "error" => p.image_error = Some("could not read that image: unsupported format".into()),
            "busy" => p.applying = true,
            // A stored / shipped wallpaper picture, as if read from disk.
            "stored" => {
                if let Ok(i) = convert_file(Path::new(val)) {
                    p.current = Some(Settings {
                        wallpaper: true,
                        ..p.current.clone().unwrap_or_default()
                    });
                    p.stored = Some(i.preview);
                }
            }
            "shipped" => {
                if let Ok(i) = convert_file(Path::new(val)) {
                    p.shipped = Some(i.preview);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn small_image_is_kept_as_is() {
        let i = convert_bytes(&png(40, 30)).unwrap();
        assert_eq!((i.width, i.height), (40, 30));
        assert_eq!(i.rgba.len(), 40 * 30 * 4);
        assert_eq!(&i.rgba[..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn wide_image_is_capped_keeping_aspect() {
        let i = convert_bytes(&png(7680, 1080)).unwrap();
        assert_eq!(i.width, ec_greeter::MAX_WIDTH);
        assert_eq!(i.height, 540);
        assert_eq!(i.rgba.len(), (i.width * i.height * 4) as usize);
    }

    #[test]
    fn tall_image_is_capped_by_height() {
        let i = convert_bytes(&png(1000, 4320)).unwrap();
        assert!(i.width <= ec_greeter::MAX_WIDTH && i.height == ec_greeter::MAX_HEIGHT);
        assert!((i.width as i64 - 500).abs() <= 1);
    }

    #[test]
    fn bad_bytes_are_an_error_not_a_panic() {
        assert!(convert_bytes(b"not an image").is_err());
        assert!(convert_bytes(&[]).is_err());
        let mut cut = png(64, 64);
        cut.truncate(40);
        assert!(convert_bytes(&cut).is_err());
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(convert_file(Path::new("/nonexistent/x.png")).is_err());
    }

    #[test]
    fn exit_codes_map_to_outcomes() {
        assert_eq!(Outcome::from_exit(Some(0)), Outcome::Done);
        assert_eq!(Outcome::from_exit(Some(126)), Outcome::Dismissed);
        assert_eq!(Outcome::from_exit(Some(127)), Outcome::NotAuthorised);
        assert_eq!(Outcome::from_exit(Some(2)), Outcome::Invalid);
        assert!(matches!(Outcome::from_exit(Some(1)), Outcome::Failed(_)));
        assert!(matches!(Outcome::from_exit(None), Outcome::Failed(_)));
    }

    type Calls = Arc<Mutex<Vec<(Vec<String>, usize)>>>;

    fn fake(code: Option<i32>) -> (Arc<Runner>, Calls) {
        let calls: Calls = Arc::default();
        let c = calls.clone();
        let r: Arc<Runner> = Arc::new(move |argv, stdin| {
            c.lock()
                .unwrap()
                .push((argv.iter().map(|s| s.to_string()).collect(), stdin.len()));
            Ok(code)
        });
        (r, calls)
    }

    #[test]
    fn apply_and_reset_use_fixed_argv() {
        let (r, calls) = fake(Some(0));
        assert_eq!(run_action(&*r, Action::Apply, b"frame"), Outcome::Done);
        assert_eq!(run_action(&*r, Action::Reset, b"ignored"), Outcome::Done);
        let c = calls.lock().unwrap();
        assert_eq!(c[0], (vec![HELPER.to_owned(), "apply".to_owned()], 5));
        assert_eq!(c[1], (vec![HELPER.to_owned(), "reset".to_owned()], 0));
    }

    #[test]
    fn runner_outcomes_and_spawn_failure() {
        let (r, _) = fake(Some(126));
        assert_eq!(run_action(&*r, Action::Apply, &[]), Outcome::Dismissed);
        let bad: Arc<Runner> = Arc::new(|_, _| Err(std::io::Error::other("no pkexec")));
        assert!(matches!(
            run_action(&*bad, Action::Reset, &[]),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn default_page_sends_the_shipped_look() {
        let p = LoginScreen::default();
        assert_eq!(p.pending(), Settings::default());
    }

    #[test]
    fn pending_reflects_the_controls() {
        let theme = LoginTheme {
            base: 0x101010,
            ink: 0xf0f0f0,
            accent: 0xe0b040,
            accent_text: 0xe0b040,
        };
        let p = LoginScreen {
            theme: Some(theme),
            use_theme: true,
            greeting: "  hello ".into(),
            clock: Some(Clock::Time12),
            wallpaper: Wallpaper::New(convert_bytes(&png(4, 4)).unwrap()),
            ..Default::default()
        };
        let s = p.pending();
        assert!(s.wallpaper);
        assert_eq!(s.theme, Some(theme));
        assert_eq!(s.greeting.as_deref(), Some("  hello "));
        assert_eq!(s.clock, Some(Clock::Time12));
        // The frame the helper parses back to the same settings and image.
        let input = ec_greeter::Input::parse(p.frame()).unwrap();
        assert_eq!(input.settings, s);
        assert_eq!(input.image.map(|(w, h, d)| (w, h, d.len())), Some((4, 4, 64)));
    }

    #[test]
    fn blank_greeting_is_the_shipped_one_and_theme_needs_the_toggle() {
        let p = LoginScreen {
            greeting: "   ".into(),
            theme: Some(LoginTheme {
                base: 0,
                ink: 0xffffff,
                accent: 0xffffff,
                accent_text: 0xffffff,
            }),
            ..Default::default()
        };
        let s = p.pending();
        assert_eq!((s.greeting, s.theme), (None, None));
    }

    #[test]
    fn keep_and_shipped_wallpaper() {
        let mut p = LoginScreen::default();
        p.adopt(Some(Settings {
            wallpaper: true,
            ..Default::default()
        }));
        assert!(p.pending().wallpaper);
        assert!(p.frame().len() < 256, "Keep sends no pixels");
        p.wallpaper = Wallpaper::Shipped;
        assert!(!p.pending().wallpaper);
    }

    #[test]
    fn greeting_is_cleaned_and_capped() {
        assert_eq!(clean_greeting("a\nb\tc"), "a b c");
        assert_eq!(clean_greeting("line1\nline2"), "line1 line2");
        // No leading space, no doubles; other controls are dropped.
        assert_eq!(clean_greeting("\n\ta\r\n\r\nb \nc\x07"), "a b c");
        let long = "x".repeat(500);
        assert_eq!(clean_greeting(&long).chars().count(), ec_greeter::MAX_GREETING);
        assert!(ec_greeter::Settings::parse(
            &Settings {
                greeting: Some(clean_greeting(&long)),
                ..Default::default()
            }
            .to_text()
        )
        .is_ok());
    }

    fn dirty_page() -> LoginScreen {
        LoginScreen {
            greeting: "hi".into(),
            ..Default::default()
        }
    }

    #[test]
    fn apply_needs_an_edit_and_reset_needs_something_stored() {
        let mut p = LoginScreen::default();
        assert!(!p.can_apply() && !p.can_reset());
        let (r, calls) = fake(Some(0));
        let _ = run(&mut p, Action::Apply, r.clone());
        let _ = run(&mut p, Action::Reset, r);
        assert!(!p.applying, "a no-op never starts a run");
        assert!(calls.lock().unwrap().is_empty());
        let mut p = dirty_page();
        assert!(p.can_apply() && !p.can_reset(), "edits alone do not enable Reset");
        p.adopt(Some(Settings {
            clock: Some(Clock::Time12),
            ..Default::default()
        }));
        assert!(!p.can_apply() && p.can_reset());
    }

    #[test]
    fn a_saved_theme_with_an_unusable_active_one_is_reachable() {
        let saved = LoginTheme {
            base: 0,
            ink: 0xffffff,
            accent: 0xffffff,
            accent_text: 0xffffff,
        };
        let mut p = LoginScreen::default();
        p.adopt(Some(Settings {
            theme: Some(saved),
            ..Default::default()
        }));
        assert!(p.use_theme && p.theme.is_none() && !p.can_apply(), "the dead end");
        // The toggle's only message turns it off, and Apply is then offered.
        p.use_theme = false;
        assert!(p.can_apply());
        assert_eq!(p.pending().theme, None);
    }

    #[test]
    fn keep_sends_no_image_even_with_a_drawn_picture() {
        let mut p = LoginScreen::default();
        p.adopt(Some(Settings {
            wallpaper: true,
            ..Default::default()
        }));
        let before = p.frame();
        p.stored = Some(convert_bytes(&png(8, 8)).unwrap().preview);
        p.shipped = p.stored.clone();
        assert_eq!(p.frame(), before);
        assert!(p.backdrop().is_some());
    }

    #[test]
    fn a_late_picture_cannot_overwrite_a_newer_one() {
        let mut app = App::new();
        let h = convert_bytes(&png(8, 8)).unwrap().preview;
        let _ = refresh_pictures(&mut app.login);
        let _ = refresh_pictures(&mut app.login);
        let late = Pictures {
            stored: Some(h.clone()),
            shipped: Some(h.clone()),
        };
        let g = app.login.picture_gen;
        let _ = update(&mut app, Msg::Pictures(g - 1, late));
        assert!(app.login.stored.is_none() && app.login.shipped.is_none());
        let now = Pictures {
            stored: Some(h),
            shipped: None,
        };
        let _ = update(&mut app, Msg::Pictures(g, now));
        assert!(app.login.stored.is_some());
    }

    #[test]
    fn shipped_wallpaper_clears_the_path_field() {
        let mut app = App::new();
        app.login.path = "/old/path.png".into();
        let _ = update(&mut app, Msg::UseShippedWallpaper);
        assert!(app.login.path.is_empty());
    }

    #[test]
    fn browse_failure_is_one_short_sentence() {
        let mut app = App::new();
        let _ = update(
            &mut app,
            Msg::Browsed(Err("file chooser portal: no session bus: zbus".into())),
        );
        assert_eq!(
            app.login.image_error.as_deref(),
            Some("No file chooser is available. Type the image's path instead.")
        );
    }

    #[test]
    fn theme_colours_come_from_validated_seeds() {
        let seeds = ec_theme::DARK_SEEDS;
        let t = login_theme(&seeds).expect("the dark seeds pass");
        assert_eq!(
            t.base,
            0xffffff & {
                let b = seeds.base;
                ((b.r() * 255.0).round() as u32) << 16
                    | ((b.g() * 255.0).round() as u32) << 8
                    | (b.b() * 255.0).round() as u32
            }
        );
        let mut bad = seeds;
        bad.ink = bad.base;
        assert!(login_theme(&bad).is_none());
    }
}
