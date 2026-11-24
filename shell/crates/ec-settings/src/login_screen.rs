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
use ec_layout::Layout;
use iced::widget::{button, column, container, row, text, text_input, Space};
use iced::{Alignment, Element, Length, Task, Theme};

use ec_ui::theme;
use ec_ui::tokens::{color, font, size, space};
use ec_ui::widget::{
    edge_note, glass_inset as inset, glass_panel as panel, inset_list, micro_label, pick_pill, pill,
    pill_group, Toggle,
};

mod layout_edit;
mod layout_view;
pub mod lock;
mod lock_mock;
mod sample;

use crate::app::{App, Message};
use crate::conn::Problem;

/// The root helper, fixed. No argument is ever built from user input.
pub const HELPER: &str = "/usr/lib/eclipse/ec-greeter-apply";
/// Refuse to read more than this of a chosen file.
const MAX_FILE: u64 = 128 * 1024 * 1024;
/// The preview thumbnail, cropped to fill this (16:9, the editor's screen).
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
    let thumb = img
        .resize_to_fill(PREVIEW.0, PREVIEW.1, image::imageops::FilterType::Triangle)
        .to_rgba8();
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
    /// Where the tiles sit (the editor's pending layout).
    pub layout: Layout,
    /// Sample data the preview's Glances draw with (display only, never sent).
    pub sample_batteries: sample::Batteries,
    pub sample_link: sample::Link,
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
            // The default layout is "nothing stored", as on disk.
            layout: (self.layout != Layout::default()).then(|| self.layout.clone()),
        }
    }

    /// What the machine has now, with a stored default layout read as none.
    fn stored(&self) -> Settings {
        let mut s = self.current.clone().unwrap_or_default();
        s.layout = s.layout.map(layout_edit::normalized);
        if s.layout.as_ref() == Some(&Layout::default()) {
            s.layout = None;
        }
        s
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
        !self.applying && self.stored() != Settings::default()
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
        self.layout = layout_edit::normalized(s.layout.clone().unwrap_or_default());
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
    /// Which sample data the preview's Glances show (display only).
    SampleBatteries(sample::Batteries),
    SampleLink(sample::Link),
    /// The layout editor finished a gesture: the whole new layout.
    SetLayout(Layout),
    /// "Reset layout": the built-in arrangement.
    ResetLayout,
    /// Apply (asks for authorisation via the compositor prompt).
    Apply,
    /// Revert: every pending edit back to what the machine has now.
    Revert,
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

/// The runner the page uses: pkexec. A debug build can swap in one that
/// succeeds without running anything (`SETTINGS_STUB_RUNNER=1`), so a
/// screenshot session can press Apply without a polkit prompt.
fn runner() -> Arc<Runner> {
    #[cfg(debug_assertions)]
    if std::env::var_os("SETTINGS_STUB_RUNNER").is_some() {
        return Arc::new(|_, _| Ok(Some(0)));
    }
    Arc::new(pkexec)
}

pub fn update(app: &mut App, m: Msg) -> Task<Message> {
    let p = &mut app.login;
    // An old result is not about what the page says now.
    if matches!(
        m,
        Msg::SetLayout(_)
            | Msg::ResetLayout
            | Msg::Greeting(_)
            | Msg::Clock(_)
            | Msg::UseTheme(_)
            | Msg::UseShippedWallpaper
            | Msg::Revert
    ) {
        p.last = None;
    }
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
        Msg::SampleBatteries(b) => p.sample_batteries = b,
        Msg::SampleLink(l) => p.sample_link = l,
        Msg::SetLayout(l) => {
            // The editor only ever sends a valid one; a bad one is dropped.
            if ec_layout::validate(&l).is_ok() {
                p.layout = l;
            }
        }
        Msg::ResetLayout => p.layout = Layout::default(),
        Msg::Apply => return run(p, Action::Apply, runner()),
        Msg::Revert => {
            if !p.applying {
                // Whatever a conversion in flight brings is stale now.
                p.generation += 1;
                p.converting = false;
                p.path.clear();
                p.image_error = None;
                let current = p.current.clone();
                p.adopt(current);
            }
        }
        Msg::Reset => return run(p, Action::Reset, runner()),
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

/// The clock preset as one line, for the row's caption: the time, then the date.
fn clock_sample(c: Option<Clock>) -> String {
    let s = sample::clock(c);
    if s.has_date {
        format!("{} \u{b7} {}", s.time, s.date)
    } else {
        s.time.to_owned()
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
    matches!(p.wallpaper, Wallpaper::New(_)) || p.pending() != p.stored()
}

/// Which of the page's edits differ from what the machine has, in the order
/// the apply bar names them.
fn changes(p: &LoginScreen) -> Vec<&'static str> {
    let (now, then) = (p.pending(), p.stored());
    let mut out = Vec::new();
    if now.layout != then.layout {
        out.push("layout");
    }
    if now.greeting != then.greeting {
        out.push("greeting");
    }
    if now.clock != then.clock {
        out.push("clock");
    }
    if matches!(p.wallpaper, Wallpaper::New(_)) || now.wallpaper != then.wallpaper {
        out.push("wallpaper");
    }
    if now.theme != then.theme {
        out.push("colours");
    }
    out
}

/// The apply bar's few words: "Layout and greeting changed". `None` when
/// nothing differs.
fn summary(p: &LoginScreen) -> Option<String> {
    let c = changes(p);
    let list = match c.as_slice() {
        [] => return None,
        [a] => (*a).to_owned(),
        [a, b] => format!("{a} and {b}"),
        [a, b, rest @ ..] if rest.len() == 1 => format!("{a}, {b} and {}", rest[0]),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    };
    let mut chars = list.chars();
    let first = chars.next()?.to_uppercase().collect::<String>();
    Some(format!("{first}{} changed", chars.as_str()))
}

/// The header's two-line chip: what the machine has now.
pub fn status(app: &App) -> (String, String) {
    let p = &app.login;
    let s = p.stored();
    let changed = [
        s.wallpaper,
        s.theme.is_some(),
        s.greeting.is_some(),
        s.clock.is_some(),
        s.layout.is_some(),
    ]
    .into_iter()
    .filter(|c| *c)
    .count();
    if changed == 0 {
        ("shipped look".into(), "nothing customised".into())
    } else {
        ("customised".into(), format!("{changed} of 5 changed"))
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

/// What picture the preview is standing in for.
fn picture_caption(p: &LoginScreen) -> String {
    match &p.wallpaper {
        Wallpaper::New(i) => format!("new image \u{b7} {}\u{d7}{}", i.width, i.height),
        Wallpaper::Shipped => "shipped wallpaper".into(),
        Wallpaper::Keep if p.current.as_ref().is_some_and(|s| s.wallpaper) => "current wallpaper".into(),
        Wallpaper::Keep => "shipped wallpaper".into(),
    }
}

/// The previewed theme's colours and the texts the page is about to send, for
/// the editor's mocks. Same source as Apply: `pending()`.
fn skin(p: &LoginScreen) -> layout_view::Skin {
    let pending = p.pending();
    // The greeter's own palette for this theme, under the scheme the desktop is in
    // (the greeter follows the compositor's), light legibility steps included.
    let pal = ec_greeter_ui::palette_for(pending.theme.as_ref(), color::palette().scheme);
    layout_view::Skin {
        base: color::to_iced(pal.base),
        ink: color::to_iced(pal.text),
        pal,
        dense: pending.wallpaper,
        clock: sample::clock(pending.clock),
        batteries: p.sample_batteries.data(),
        link: p.sample_link.quality(),
        greeting: pending
            .greeting
            .unwrap_or_else(|| ec_ui::tokens::layout_edit::MOCK_GREETING.to_owned()),
        backdrop: p.backdrop().cloned(),
    }
}

/// The layout editor on its own, with the preview's sample data named as the
/// greeter's preview example names it (`none|one|several`, `none|strong|weak`),
/// for `examples/builder_preview.rs`: the builder's tiles and the greeter's,
/// rendered from the same layout and data, side by side. Not used by the app.
pub fn preview_editor<'a, M: Clone + 'a>(
    layout: &'a Layout,
    theme: Option<LoginTheme>,
    clock: Option<Clock>,
    batteries: &str,
    link: &str,
    on_change: impl Fn(Layout) -> M + 'a,
) -> Element<'a, M, Theme> {
    let pal = ec_greeter_ui::palette_for(theme.as_ref(), color::palette().scheme);
    let skin = layout_view::Skin {
        base: color::to_iced(pal.base),
        ink: color::to_iced(pal.text),
        pal,
        dense: false,
        clock: sample::clock(clock),
        batteries: match batteries {
            "none" => sample::Batteries::None,
            "several" => sample::Batteries::Several,
            _ => sample::Batteries::One,
        }
        .data(),
        link: match link {
            "none" => sample::Link::None,
            "weak" => sample::Link::Weak,
            _ => sample::Link::Strong,
        }
        .quality(),
        greeting: ec_ui::tokens::layout_edit::MOCK_GREETING.to_owned(),
        backdrop: None,
    };
    layout_view::editor(layout, skin, on_change).into()
}

/// The hero: the login screen itself as a live editor, at its proportions,
/// in a glass panel. Drag a tile to move it, pull its corner to resize it,
/// tap a chip in the tray to add one. A canvas, not a list.
fn hero(app: &App) -> Element<'_, Message, Theme> {
    let p = &app.login;
    let edited = p.layout != p.stored().layout.unwrap_or_default();
    let n = p.layout.tiles.len();
    let state = if edited {
        format!("{n} tiles \u{b7} edited")
    } else {
        format!("{n} tiles")
    };
    let reset = button(
        small("Reset layout")
            .font(font::UI_MEDIUM)
            .wrapping(text::Wrapping::None),
    )
    .padding([space::PILL_Y, space::PILL_X])
    .on_press_maybe((p.layout != Layout::default()).then(|| msg(Msg::ResetLayout)))
    .style(theme::link);
    panel(
        app.glass_radius,
        app.hairline_cards,
        column![
            row![
                micro_label("login screen"),
                container(mono_tertiary(format!("{state} \u{b7} {}", picture_caption(p))))
                    .width(Length::Fill)
                    .align_x(Alignment::End),
                reset,
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
            container(
                layout_view::editor(&p.layout, skin(p), |l| msg(Msg::SetLayout(l))).bar(layout_view::Bar {
                    summary: summary(p),
                    note: p.last.as_ref().map(|(a, o)| (o.message(*a), o.is_success())),
                    can_apply: p.can_apply(),
                    applying: p.applying,
                    on_apply: msg(Msg::Apply),
                    on_revert: msg(Msg::Revert),
                })
            )
            .width(Length::Fill)
            .align_x(Alignment::Center),
            samples(p),
            small(
                "Drag a tile to move it; pull any edge or corner to resize it. Tap or drag a tile below to add it. \
                 Nothing changes on this computer until you press Apply."
            )
            .style(theme::text_tertiary),
        ]
        .extend(result_banner(p))
        .spacing(space::ROW_Y),
    )
}

/// What the preview's Glances pretend the machine has, so a tile's sizing can be
/// seen as the data changes. Display only: nothing here is applied.
fn samples(p: &LoginScreen) -> Element<'_, Message, Theme> {
    let batteries = sample::Batteries::ALL
        .into_iter()
        .map(|b| {
            pick_pill(
                b.label(),
                p.sample_batteries == b,
                Some(msg(Msg::SampleBatteries(b))),
            )
        })
        .collect();
    let link = sample::Link::ALL
        .into_iter()
        .map(|l| pick_pill(l.label(), p.sample_link == l, Some(msg(Msg::SampleLink(l)))))
        .collect();
    let line = |name: &'static str, pills| {
        row![
            container(small(name).style(theme::text_tertiary)).width(Length::Fixed(sample_label_w())),
            container(pill_group(pills)).width(Length::Fill),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center)
    };
    column![line("Preview battery", batteries), line("Preview Wi-Fi", link)]
        .spacing(space::PILL_GAP)
        .into()
}

fn sample_label_w() -> f32 {
    ec_ui::tokens::layout_edit::SAMPLE_LABEL_W
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

/// The quiet Reset and the one line about the password prompt. Apply itself
/// lives in the editor's bar, where the edits are.
fn actions(p: &LoginScreen) -> Element<'_, Message, Theme> {
    let reset = button(small("Reset to default").font(font::UI_MEDIUM))
        .padding([space::PILL_Y, space::PILL_X])
        .on_press_maybe(p.can_reset().then(|| msg(Msg::Reset)))
        .style(theme::link);
    column![
        reset,
        small(
            "Apply asks for an administrator password in a system prompt drawn by the compositor, \
             not here. It changes the login screen for every user of this computer.",
        )
        .style(theme::text_tertiary),
    ]
    .spacing(space::ROW_Y)
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
/// inset list of hairlined rows -> the unboxed Reset line. Apply is the editor's own
/// bar, so it is where the edits are; the result banner (inset, only after a
/// run) sits under it, where the person just clicked. No two adjacent
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
/// `theme` (use the dark seeds), `light` (the light seeds), `notheme` (the active theme is unusable),
/// `greeting:<text>`, `long` (an 80-character greeting), `clock:<0-3>`,
/// `layout:<default|bw|bare>` (the pending layout: default; battery and wifi
/// added; every optional tile removed),
/// `bat:<none|one|several>`, `link:<none|strong|weak>`, `tiles:<kind x y w h/..>` (sample data and
/// an exact layout), `ok`, `fail`, `error`, `busy`.
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
            "light" => {
                p.theme = login_theme(&ec_theme::LIGHT_SEEDS);
                p.use_theme = true;
            }
            "notheme" => {
                p.theme = None;
                p.use_theme = false;
            }
            "greeting" => p.greeting = clean_greeting(val),
            "long" => p.greeting = clean_greeting(&"Welcome back, good to see you. ".repeat(5)),
            "clock" => p.clock = val.parse::<usize>().ok().and_then(|n| Clock::ALL.get(n).copied()),
            "layout" => {
                let mut l = Layout::default();
                match val {
                    "bw" => {
                        for k in [ec_layout::Kind::Battery, ec_layout::Kind::Wifi] {
                            l = layout_edit::add(layout_edit::Screen::Login, &l, k, None).unwrap_or(l);
                        }
                    }
                    // Every tile at its smallest, for the mocks' degradation.
                    "tiny" => {
                        for tile in l.tiles.iter_mut().filter(|t| !t.kind.is_intrinsic()) {
                            let (w, h) = tile.kind.min_size();
                            tile.rect = ec_layout::Rect::new(tile.rect.x, tile.rect.y, w, h);
                        }
                    }
                    "bare" => {
                        for k in layout_edit::OPTIONAL {
                            l = layout_edit::remove(layout_edit::Screen::Login, &l, k).unwrap_or(l);
                        }
                    }
                    _ => {}
                }
                p.layout = l;
            }
            // The sample data the preview's Glances draw with.
            "bat" => {
                p.sample_batteries = match val {
                    "none" => sample::Batteries::None,
                    "several" => sample::Batteries::Several,
                    _ => sample::Batteries::One,
                }
            }
            "link" => {
                p.sample_link = match val {
                    "none" => sample::Link::None,
                    "weak" => sample::Link::Weak,
                    _ => sample::Link::Strong,
                }
            }
            // `tiles:clock 2 1 10 3/battery 14 1 8 5`: an exact layout.
            "tiles" => {
                let tiles = val
                    .split('/')
                    .filter_map(|t| {
                        let mut it = t.split_whitespace();
                        let kind = ec_layout::Kind::from_name(it.next()?)?;
                        let mut n = || it.next()?.parse::<i32>().ok();
                        Some(ec_layout::Tile {
                            kind,
                            rect: ec_layout::Rect::new(n()?, n()?, n()?, n()?),
                        })
                    })
                    .collect();
                let l = Layout { tiles };
                if ec_layout::validate(&l).is_ok() {
                    p.layout = l;
                }
            }
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
    fn the_bar_names_what_changed_in_a_few_words() {
        let mut p = LoginScreen::default();
        assert_eq!(summary(&p), None);
        p.layout =
            layout_edit::remove(layout_edit::Screen::Login, &p.layout, ec_layout::Kind::Power).unwrap();
        assert_eq!(summary(&p).as_deref(), Some("Layout changed"));
        p.greeting = "hi".into();
        assert_eq!(summary(&p).as_deref(), Some("Layout and greeting changed"));
        p.clock = Some(Clock::Time24);
        assert_eq!(summary(&p).as_deref(), Some("Layout, greeting and clock changed"));
        p.wallpaper = Wallpaper::New(convert_bytes(&png(4, 4)).unwrap());
        assert_eq!(
            summary(&p).as_deref(),
            Some("Layout, greeting and 2 more changed")
        );
    }

    #[test]
    fn revert_returns_every_pending_edit_to_the_applied_state() {
        let mut app = App::new();
        app.login.adopt(Some(Settings {
            greeting: Some("stored".into()),
            ..Settings::default()
        }));
        app.login.greeting = "edited".into();
        app.login.clock = Some(Clock::Date12);
        app.login.layout = layout_edit::remove(
            layout_edit::Screen::Login,
            &app.login.layout,
            ec_layout::Kind::Power,
        )
        .unwrap();
        app.login.wallpaper = Wallpaper::Shipped;
        assert!(dirty(&app.login));
        let _ = update(&mut app, Msg::Revert);
        assert!(!dirty(&app.login));
        assert_eq!(app.login.greeting, "stored");
        assert_eq!(summary(&app.login), None);
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

    #[test]
    fn the_layout_is_part_of_pending_and_the_default_is_nothing_stored() {
        let mut p = LoginScreen::default();
        assert_eq!(p.pending().layout, None);
        assert!(!p.can_apply());
        p.layout = layout_edit::add(
            layout_edit::Screen::Login,
            &p.layout,
            ec_layout::Kind::Battery,
            None,
        )
        .unwrap();
        let s = p.pending();
        assert_eq!(s.layout.as_ref(), Some(&p.layout));
        assert!(p.can_apply());
        // The frame the helper re-validates parses back to the same layout.
        let input = ec_greeter::Input::parse(p.frame()).unwrap();
        assert_eq!(input.settings.layout, s.layout);
        // Back to the default: nothing to send again.
        p.layout = Layout::default();
        assert!(!p.can_apply());
    }

    #[test]
    fn a_stored_default_layout_is_not_a_change_and_not_resettable() {
        let mut p = LoginScreen::default();
        p.adopt(Some(Settings {
            layout: Some(Layout::default()),
            ..Default::default()
        }));
        assert!(!p.can_apply() && !p.can_reset());
        p.layout =
            layout_edit::remove(layout_edit::Screen::Login, &p.layout, ec_layout::Kind::Power).unwrap();
        assert!(p.can_apply());
    }

    #[test]
    fn set_layout_keeps_only_valid_layouts_and_reset_restores_the_default() {
        let mut app = App::new();
        let ok = layout_edit::add(
            layout_edit::Screen::Login,
            &Layout::default(),
            ec_layout::Kind::Wifi,
            None,
        )
        .unwrap();
        let _ = update(&mut app, Msg::SetLayout(ok.clone()));
        assert_eq!(app.login.layout, ok);
        let mut bad = ok.clone();
        bad.set(ec_layout::Kind::Clock, ec_layout::Rect::new(8, 5, 8, 3));
        let _ = update(&mut app, Msg::SetLayout(bad));
        assert_eq!(app.login.layout, ok, "an overlapping layout is dropped");
        let _ = update(&mut app, Msg::ResetLayout);
        assert_eq!(app.login.layout, Layout::default());
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
