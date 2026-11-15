// SPDX-License-Identifier: AGPL-3.0-only
//! State, messages and view.
//!
//! Every target is one glass sheet, centred: a glyph disc, one sentence and
//! what it is for, then (Terminal only) a provenance chip with folded details,
//! then the starred field (or, for the yes/no and display modes, the code
//! itself) and the pills. The window is fitted to the sheet's measured height
//! on every change (min == max), so there is no dead space and nothing is cut.
//!
//! Accent ledger: the one yellow is the committing pill ("Connect" / "Pair" /
//! "Save" / "Allow"). A `Show` window commits nothing, so there it moves to
//! the code the human has to type; an unchecked Terminal request spends it on
//! its "Not from Apotheosis" chip and leaves the pill plain. A refusal is
//! `DANGER`, which is not the accent.

use std::fmt;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::widget::{container, row, scrollable, sensor, text, text_input, Column, Row, Space};
use iced::{Alignment, Color, Element, Length, Subscription, Task, Theme};

use ec_ui::motion::{Animated, Curve, Motion};
use ec_ui::theme;
use ec_ui::theme::CellTone;
use ec_ui::tokens::{font, radius, secret, size, space};
use ec_ui::widget::{
    disclosure, glass_card, glyph_disc, mono_words, provenance_chip, secret_field_pane, sheet_pill,
    starred_input, Fade, FieldState, Reveal,
};

use crate::terminal::TerminalAsk;
use crate::{clean, clean_tail, service, Buffer, Target};

const FIELD: &str = "secret";
/// The band's mono marker. A caret, not a word: at prompt scale a word
/// reads as placeholder text.
const MARKER: &str = "›";
/// The note when the field is at its longest and refused a keystroke.
const FULL: &str = "Maximum length reached.";
/// The field's placeholder once its value is with the service.
const SENT: &str = "sent";
/// The field's placeholder once the service has kept the value.
const STORED: &str = "stored";
/// How long a stored API key's window says so before it closes: long
/// enough to read one word, short enough not to be a second dialog.
const SAVED_FOR: std::time::Duration = std::time::Duration::from_millis(900);

#[derive(Clone)]
pub enum Message {
    /// The compositor's theme moved and the palette was re-applied (ADR 0084);
    /// nothing to do but let the views rebuild.
    ThemeChanged,
    /// The field's whole new value. Never `Debug`-printed: see the impl.
    Typed(String),
    Submit,
    /// The service's verdict on the one attempt in flight.
    Done(Result<(), String>),
    Cancel,
    /// The success note has been read; the window goes.
    Close,
    /// Open or fold a Terminal sheet's details, or their second level.
    Details,
    More,
    /// The sheet's natural size: the window is fitted to it.
    Measured(iced::Size),
    /// Bind compositor glass to the window, and its answer.
    GlassWindow(iced::window::Id),
    GlassReady(bool),
    /// A frame while something moves.
    Frame(Instant),
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::Typed(_) => f.write_str("Typed(<redacted>)"),
            Message::Submit => f.write_str("Submit"),
            Message::Done(r) => write!(f, "Done({r:?})"),
            Message::Cancel => f.write_str("Cancel"),
            Message::Close => f.write_str("Close"),
            Message::ThemeChanged => f.write_str("ThemeChanged"),
            Message::Details => f.write_str("Details"),
            Message::More => f.write_str("More"),
            Message::Measured(s) => write!(f, "Measured({s:?})"),
            Message::GlassWindow(id) => write!(f, "GlassWindow({id:?})"),
            Message::GlassReady(on) => write!(f, "GlassReady({on})"),
            Message::Frame(_) => f.write_str("Frame"),
        }
    }
}

pub struct App {
    target: Target,
    typed: Buffer,
    /// The last refusal, in words. Never contains the secret.
    problem: Option<String>,
    /// An attempt is with the service. A second Submit waits for its answer
    /// rather than racing it.
    pending: bool,
    /// The last keystroke was refused because the field is at its longest; the
    /// note says so instead of the field going silently deaf.
    full: bool,
    /// The service took it. Only an API key window lingers on this (the
    /// key has no other visible effect); the rest close at once.
    saved: bool,
    /// A Terminal sheet's details are open (from the start when unchecked),
    /// and their second level (pids, uids, the pane's titles).
    details: bool,
    more: bool,
    /// The compositor draws its blur behind the window: the ground is the live
    /// window tint, else opaque.
    blur: bool,
    /// The sheet's glyph, looked up once.
    icon: Option<PathBuf>,
    /// The window height last asked for.
    height: f32,
    /// The open (content fades and rises in), the details' and More's
    /// unfold: `0.0..=1.0`, eased. Snap under reduce-motion.
    entrance: Animated,
    unfold: Animated,
    unfold_more: Animated,
    /// How long a cell's hover and press take; zero under reduce-motion.
    hover: Duration,
}

/// `ms` of ease-out, or a snap when Abyss asks for reduced motion.
fn motion(reduce: bool, ms: u64) -> Motion {
    if reduce {
        Motion::SNAP
    } else {
        Motion {
            enabled: true,
            curve: Curve::EaseOut,
            duration: Duration::from_millis(ms),
        }
    }
}

fn amount(open: bool) -> f32 {
    f32::from(u8::from(open))
}

/// The icon theme the sheet's glyph comes from: the set guaranteed complete
/// for the standard symbolic names, as the bar asks it.
const ICON_THEME: &str = "Adwaita";
/// The lookup size; the SVG is drawn at `secret::DISC_GLYPH`.
const ICON_SIZE: u16 = 24;

fn symbolic(name: &str) -> Option<PathBuf> {
    freedesktop_icons::lookup(&format!("{name}-symbolic"))
        .with_size(ICON_SIZE)
        .with_theme(ICON_THEME)
        .with_cache()
        .find()
}

impl App {
    pub fn new(target: Target) -> (Self, Task<Message>) {
        let (_, blur) = ec_ui::ipc::fetch_glass();
        let blur = blur.unwrap_or(false);
        if blur {
            ec_ui::ipc::sync_surface_now();
        }
        let details = matches!(&target, Target::Terminal(t) if !t.verified);
        let icon = symbolic(target.glyph());
        let height = window_size(&target).height;
        let reduce = ec_ui::ipc::fetch_reduce_motion().unwrap_or(false);
        let mut entrance = Animated::new(0.0, motion(reduce, secret::IN_MS));
        entrance.set_target(1.0, Instant::now());
        let unfold_motion = motion(reduce, secret::UNFOLD_MS);
        #[allow(unused_mut)]
        let mut app = Self {
            entrance,
            unfold: Animated::new(amount(details), unfold_motion),
            unfold_more: Animated::new(0.0, unfold_motion),
            hover: if reduce {
                Duration::ZERO
            } else {
                Duration::from_millis(secret::HOVER_MS)
            },
            details,
            more: false,
            blur,
            icon,
            height,
            target,
            typed: Buffer::default(),
            problem: None,
            full: false,
            pending: false,
            saved: false,
        };
        // Debug-build fixture: open already waiting on the service, so the
        // pending state can be looked at without sending anything anywhere.
        #[cfg(debug_assertions)]
        match std::env::var("SECRET_PROMPT_PREVIEW").as_deref() {
            Ok("pending") => app.pending = true,
            Ok("saved") => app.saved = true,
            // A dummy value, so the masking can be looked at. Never a real secret.
            Ok("typed") => app.typed.replace("x".repeat(9)),
            Ok("typed-long" | "typed-select") => app.typed.replace("x".repeat(70)),
            Ok("full") => {
                app.typed.replace("x".repeat(crate::terminal::MAX_SECRET));
                app.full = true;
            }
            Ok("details") => {
                app.details = true;
                app.unfold.snap(1.0);
            }
            Ok("more") => {
                (app.details, app.more) = (true, true);
                app.unfold.snap(1.0);
                app.unfold_more.snap(1.0);
            }
            Ok("refused") => app.problem = Some("The secret store is locked. Unlock it first.".to_owned()),
            _ => {}
        }
        // The window exists for one keystroke sequence; it opens ready for it.
        #[allow(unused_mut)]
        let mut task = iced::widget::operation::focus(FIELD);
        #[cfg(debug_assertions)]
        if std::env::var("SECRET_PROMPT_PREVIEW").as_deref() == Ok("typed-select") {
            task = task.chain(iced::widget::operation::select_range(FIELD, 40, 58));
        }
        #[cfg(debug_assertions)]
        if let Ok(path) = std::env::var("SECRET_PROMPT_SHOT") {
            task = Task::batch([task, preview_shot(path)]);
        }
        (app, task)
    }

    pub fn title(&self) -> &'static str {
        self.target.title()
    }
}

/// Debug builds only: read the window's frame back after a moment, write it
/// to `path` as `WxH\n` and raw RGBA, and exit — the screenshot loop on a
/// host with no screencopy client. The field is empty in every fixture, so
/// nothing secret can land in the file.
#[cfg(debug_assertions)]
fn preview_shot(path: String) -> Task<Message> {
    let (tx, rx) = iced::futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(3250));
        let _ = tx.send(());
    });
    Task::perform(
        async move {
            let _ = rx.await;
        },
        |()| (),
    )
    .then(|()| iced::window::latest())
    .and_then(iced::window::screenshot)
    .then(move |shot| {
        let head = format!("{}x{}\n", shot.size.width, shot.size.height);
        let _ = std::fs::write(&path, [head.as_bytes(), &shot.rgba[..]].concat());
        iced::exit()
    })
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::ThemeChanged => return Task::none(),
        Message::Details => {
            app.details = !app.details;
            app.unfold.set_target(amount(app.details), Instant::now());
        }
        Message::More => {
            app.more = !app.more;
            app.unfold_more.set_target(amount(app.more), Instant::now());
        }
        Message::GlassWindow(id) => return ec_ui::glass::attach_window(id).map(Message::GlassReady),
        // Any update redraws, and that frame carries the panes.
        Message::GlassReady(_) => {}
        Message::Frame(now) => {
            app.entrance.tick(now);
            app.unfold.tick(now);
            app.unfold_more.tick(now);
        }
        Message::Measured(natural) => {
            let h = (natural.height + 2.0 * space::PANE_Y)
                .ceil()
                .min(secret::SHEET_MAX_H);
            if (h - app.height).abs() < 1.0 {
                return Task::none();
            }
            app.height = h;
            return fit(iced::Size::new(secret::SHEET_W, h));
        }
        Message::Typed(new) => {
            if app.target.admits(&new) {
                app.typed.replace(new);
                app.problem = None;
                app.full = false;
            } else {
                // Only a length cap is worth a word; a passkey field refusing a
                // letter is self-evident.
                app.full = matches!(app.target, Target::Terminal(_));
                crate::wipe(new);
            }
        }
        Message::Submit => {
            if app.pending || !app.target.ready(app.typed.as_str()) {
                return Task::none();
            }
            // A display-only window has nothing to send: "Done" just closes.
            if !app.target.answers() {
                return iced::exit();
            }
            // A refused secret is not kept for a second try: the human
            // retypes it, and we hold it for no longer than the one attempt.
            // The join can take tens of seconds, so it runs off this thread.
            app.pending = true;
            let secret = app.typed.take();
            let target = app.target.clone();
            let (tx, rx) = iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(service::submit(&target, secret));
            });
            return Task::perform(
                async move {
                    rx.await
                        .unwrap_or_else(|_| Err("The attempt was lost.".to_owned()))
                },
                Message::Done,
            );
        }
        Message::Done(Ok(())) if matches!(app.target, Target::ApiKey { .. }) => {
            app.pending = false;
            app.saved = true;
            let (tx, rx) = iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                std::thread::sleep(SAVED_FOR);
                let _ = tx.send(());
            });
            return Task::perform(async move { rx.await.ok() }, |_| Message::Close);
        }
        Message::Done(Ok(())) => return iced::exit(),
        Message::Close => return iced::exit(),
        Message::Done(Err(problem)) => {
            app.pending = false;
            app.problem = Some(problem);
            return iced::widget::operation::focus(FIELD);
        }
        // Saved, and on its way out: nothing left to refuse.
        Message::Cancel if app.saved => return iced::exit(),
        Message::Cancel => {
            app.typed.clear();
            // No-op for a `Show` window: there is no request to refuse.
            service::reject(&app.target);
            return iced::exit();
        }
    }
    Task::none()
}

/// The size the window opens at, before the sheet is first measured.
pub fn window_size(target: &Target) -> iced::Size {
    let h = match target {
        Target::Terminal(t) if !t.verified => secret::SHEET_H_EST_OPEN,
        Target::Terminal(_) => secret::SHEET_H_EST_CHIP,
        _ => secret::SHEET_H_EST,
    };
    iced::Size::new(secret::SHEET_W, h)
}

/// Resize the window to `size` and pin it there (min == max), so it is never
/// user-resizable and abyss keeps floating it. A snap: a stepped resize would
/// be one configure round-trip per frame, each re-laying the whole sheet.
fn fit(size: iced::Size) -> Task<Message> {
    use iced::window;
    window::latest().and_then(move |id| {
        window::set_min_size(id, None)
            .chain(window::set_max_size(id, None))
            .chain(window::resize(id, size))
            .chain(window::set_min_size(id, Some(size)))
            .chain(window::set_max_size(id, Some(size)))
    })
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    // The sheet is measured at its natural height inside a scroller, so the
    // measure never depends on the window it sizes; past the cap it scrolls.
    // Panes inside follow the scroll and are cut to it.
    let measured = sensor(sheet(app)).on_resize(Message::Measured);
    let body = ec_ui::glass::scroll_region(
        scrollable(measured)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(space::SCROLLBAR_W)
                    .scroller_width(space::SCROLLBAR_W),
            ))
            .style(theme::eclipse_scrollable)
            .height(Length::Fill),
    );
    let shown = app.entrance.value();
    // The content rises into place; the window's ground is not moved.
    let body = Fade::new(body, shown, secret::RISE, Color::TRANSPARENT);
    // The ground: a compositor glass pane when abyss draws one; else, with
    // blur on, the live window tint over the compositor's blur (its corner
    // mask rounds it), with blur off the opaque base.
    let blur = app.blur;
    let sheet = glass_card(
        container(body)
            .padding([space::PANE_Y, space::PANE_X])
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |th: &Theme| theme::bare_container_if_glass(ground(blur)(th))),
        CellTone::Plain,
        radius::WINDOW,
    );
    // The whole sheet fades in under a wash of its own ground, so nothing
    // darker than the ground shows at an edge.
    ec_ui::glass::root_sole(Fade::new(sheet, shown, 0.0, wash(blur))).into()
}

/// The window's ground style.
fn ground(blur: bool) -> impl Fn(&Theme) -> iced::widget::container::Style {
    move |th: &Theme| {
        if blur {
            theme::window_pane(true)(th)
        } else {
            theme::window(radius::WINDOW)(th)
        }
    }
}

/// The ground's colour, for a fade to wash its content with — only for the
/// client-drawn fallback. On compositor glass the sheet is translucent and
/// a wash would flash an opaque slab; there the fade is the panes' and the
/// compositor's own open animation.
fn wash(blur: bool) -> Color {
    if ec_ui::glass::available() {
        return Color::TRANSPARENT;
    }
    match ground(blur)(&Theme::Dark).background {
        Some(iced::Background::Color(c)) => c,
        _ => Color::TRANSPARENT,
    }
}

/// The field's placeholder: where the value went, once it has gone.
fn hint(app: &App) -> &'static str {
    match (app.saved, app.pending) {
        (true, _) => STORED,
        (false, true) => SENT,
        (false, false) => app.target.hint(),
    }
}

/// The note left of the pills (only when something needs saying), then the
/// pills.
fn actions(app: &App) -> Element<'_, Message, Theme> {
    let t = &app.target;
    let small = |s: String, ink: fn(&Theme) -> text::Style| -> Element<'_, Message, Theme> {
        text(s).size(size::BODY_SMALL).style(ink).into()
    };
    let note: Element<'_, Message, Theme> = match &app.problem {
        Some(p) => small(p.clone(), theme::text_danger),
        None if app.full => small(FULL.to_owned(), theme::text_secondary),
        None => Space::new().into(),
    };

    let verb = if app.pending { t.working() } else { t.verb() };
    // An unchecked Terminal request spends the yellow on its chip, not here.
    let lit = !matches!(t, Target::Terminal(a) if !a.verified);
    let mut actions = row![container(note).width(Length::Fill)]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    if app.saved {
        // Done is the only thing left to say; the window is already going.
        actions = actions.push(sheet_pill("Done", false, Message::Close, app.hover));
    } else if t.answers() {
        actions = actions
            .push(sheet_pill("Cancel", false, Message::Cancel, app.hover))
            .push(sheet_pill(verb, lit, Message::Submit, app.hover));
    } else {
        actions = actions.push(sheet_pill(verb, false, Message::Submit, app.hover));
    }
    actions.into()
}

// Text caps, in characters, sized to the fixed sheet (`secret::SHEET_W`) so an
// unbroken token can never widen a line past it.
/// The command or host after the heading's "to run" / "for".
const PURPOSE_MAX: usize = 38;
/// A pane title in the chip.
const PANE_MAX: usize = 18;
/// A detail's value.
const VALUE_MAX: usize = 32;
/// The full command line in the details, wrapped.
const ARGV_MAX: usize = 150;
/// Pids shown in the ancestry.
const CHAIN_MAX: usize = 6;

/// `sudo` options that take a value, so the command after them can be found.
const SUDO_VALUED: &[&str] = &[
    "-u",
    "-g",
    "-C",
    "-D",
    "-h",
    "-p",
    "-r",
    "-t",
    "-T",
    "-U",
    "--user",
    "--group",
    "--close-from",
    "--chdir",
    "--host",
    "--prompt",
    "--role",
    "--type",
    "--command-timeout",
    "--other-user",
];

/// The program's arguments as a person reads them: for `sudo`, the command it
/// will run (its own options skipped); otherwise everything after argv[0].
fn command_after_program(argv: &[String]) -> &[String] {
    let Some((first, rest)) = argv.split_first() else {
        return &[];
    };
    if first.rsplit('/').next() != Some("sudo") {
        return rest;
    }
    let mut i = 0;
    while let Some(a) = rest.get(i) {
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a.len() < 2 {
            break;
        }
        i += if SUDO_VALUED.contains(&a.as_str()) { 2 } else { 1 };
    }
    rest.get(i..).unwrap_or(&[])
}

/// argv as one line, an argument with a space in it (or none at all) quoted so
/// the words stay where the program put them.
fn command_line(argv: &[String], max: usize) -> String {
    let words: Vec<String> = argv
        .iter()
        .map(|a| {
            if a.is_empty() || a.chars().any(char::is_whitespace) {
                format!("'{a}'")
            } else {
                a.clone()
            }
        })
        .collect();
    clean(&words.join(" "), max)
}

/// The heading's second line: what the password is for. `("for", host)` for a
/// host, else `("to run", command)`; `None` when the program said neither.
fn purpose(ask: &TerminalAsk) -> Option<(&'static str, String)> {
    let c = &ask.claims;
    if let Some(host) = c
        .host
        .as_deref()
        .map(|h| clean(h, PURPOSE_MAX))
        .filter(|h| !h.is_empty())
    {
        return Some(("for", host));
    }
    let cmd = command_line(command_after_program(&c.argv), PURPOSE_MAX);
    (!cmd.is_empty()).then_some(("to run", cmd))
}

/// The chip's line: which pane the request came from.
fn provenance(ask: &TerminalAsk) -> String {
    let f = &ask.facts;
    if f.pane_id.is_none() {
        return "From an Apotheosis link".to_owned();
    }
    let title = [&f.pane, &f.tab]
        .into_iter()
        .map(|t| clean(t, PANE_MAX))
        .find(|t| !t.is_empty());
    match title {
        Some(t) => format!("From your {t} pane"),
        None => "From your terminal".to_owned(),
    }
}

/// A path under `$HOME` as `~/…`.
fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && (path == home || path.starts_with(&format!("{home}/"))) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_owned(),
    }
}

/// Who a uid is, from where the human sits. No numbers: those are one level
/// further in.
fn who(uid: u32) -> &'static str {
    match uid {
        0 => "root",
        u if u == rustix::process::getuid().as_raw() => "you",
        _ => "another user",
    }
}

/// One detail: what it is, and its value. `Warn` reads as a caution, `Mono`
/// is the command itself (the one monospace line).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ink {
    Plain,
    Warn,
    Mono,
}

/// The details a person decides by, then (with `more`) the ones a person
/// checks by.
fn facts(ask: &TerminalAsk, more: bool) -> Vec<(&'static str, String, Ink)> {
    let f = &ask.facts;
    let c = &ask.claims;
    let mut rows = Vec::new();
    if !c.argv.is_empty() {
        rows.push(("Command", command_line(&c.argv, ARGV_MAX), Ink::Mono));
    }
    let program = match (&f.exe, ask.verified) {
        (Some(exe), true) => clean_tail(exe, VALUE_MAX),
        _ => clean(ask.program(), VALUE_MAX),
    };
    rows.push(("Program", program, Ink::Plain));
    rows.push(("Runs as", who(f.euid).to_owned(), Ink::Plain));
    rows.push(if f.in_foreground {
        ("Foreground", "Yes".to_owned(), Ink::Plain)
    } else {
        ("Foreground", "No, in the background".to_owned(), Ink::Warn)
    });
    if let Some(cwd) = c.cwd.as_deref().filter(|d| !d.is_empty()) {
        rows.push(("Folder", clean_tail(&tilde(cwd), VALUE_MAX), Ink::Plain));
    }
    let prompt = clean(&c.prompt, VALUE_MAX);
    if !prompt.is_empty() {
        rows.push(("Prompt", prompt, Ink::Plain));
    }
    if more {
        let titles: Vec<String> = [&f.window, &f.tab, &f.pane]
            .into_iter()
            .map(|t| clean(t, PANE_MAX))
            .filter(|t| !t.is_empty())
            .collect();
        if !titles.is_empty() {
            rows.push(("Pane", clean(&titles.join(" › "), VALUE_MAX), Ink::Plain));
        }
        rows.push(("Process", format!("pid {}", f.pid), Ink::Plain));
        rows.push(("User ids", format!("uid {} · euid {}", f.uid, f.euid), Ink::Plain));
        if !f.ancestry.is_empty() {
            let mut chain: Vec<String> = f.ancestry.iter().take(CHAIN_MAX).map(u32::to_string).collect();
            if f.ancestry.len() > CHAIN_MAX {
                chain.push("…".to_owned());
            }
            rows.push(("Parents", chain.join(" ← "), Ink::Plain));
        }
    }
    rows
}

/// The open details: a quiet inset of label / value lines, then the second
/// level's disclosure.
fn details<'a>(app: &'a App, ask: &'a TerminalAsk) -> Element<'a, Message, Theme> {
    let row_of = |(label, value, ink): (&'static str, String, Ink)| -> Element<'a, Message, Theme> {
        let v: Element<'a, Message, Theme> = match ink {
            Ink::Plain => text(value)
                .size(size::BODY_SMALL)
                .style(theme::text_secondary)
                .wrapping(text::Wrapping::None)
                .into(),
            Ink::Warn => text(value)
                .size(size::BODY_SMALL)
                .style(theme::text_danger)
                .wrapping(text::Wrapping::None)
                .into(),
            // The command breaks between its words, inside one only when it
            // is longer than the line.
            Ink::Mono => mono_words(&value, theme::text_secondary),
        };
        row![
            text(label)
                .size(size::BODY_SMALL)
                .style(theme::text_tertiary)
                .width(Length::Fixed(secret::FACT_LABEL_W)),
            container(v).width(Length::Fill),
        ]
        .align_y(Alignment::Start)
        .into()
    };
    let base = facts(ask, false);
    let n = base.len();
    let mut list = Column::new().spacing(secret::DETAILS_ROW_GAP);
    for f in base {
        list = list.push(row_of(f));
    }
    // The second level unfolds inside the first, and stays laid out until
    // its fold has played.
    let more = app.unfold_more.value();
    if app.more || more > 0.0 {
        let mut extra = Column::new().spacing(secret::DETAILS_ROW_GAP);
        for f in facts(ask, true).into_iter().skip(n) {
            extra = extra.push(row_of(f));
        }
        list = list.push(Reveal::new(Fade::new(extra, more, 0.0, wash(app.blur)), more));
    }
    list = list.push(row![
        Space::new().width(Length::Fixed(secret::FACT_LABEL_W)),
        disclosure(
            if app.more { "Less" } else { "More" },
            more,
            Message::More,
            app.hover
        ),
    ]);
    glass_card(
        container(list)
            .padding(secret::DETAILS_PAD)
            .width(Length::Fill)
            .style(|t| theme::bare_container_if_glass(theme::inset(t))),
        CellTone::Plain,
        radius::INSET,
    )
}

/// The sheet: the glyph disc, one sentence and what it is for, the Terminal
/// provenance chip and details, the field or the code, the pills. Natural
/// height: the window is fitted to it.
///
/// Silhouettes: a disc, centred text, (a capsule row, a quiet inset), the glass
/// field or a magnitude, the pill row. A Terminal heading and every detail are
/// program text, cleaned and capped.
fn sheet(app: &App) -> Element<'_, Message, Theme> {
    let t = &app.target;
    let mut col = Column::new().align_x(Alignment::Center).width(Length::Fill);
    col = col
        .push(glyph_disc(app.icon.clone(), secret::DISC, secret::DISC_GLYPH))
        .push(Space::new().height(secret::DISC_GAP))
        .push(
            text(t.heading())
                .font(font::UI_SEMIBOLD)
                .size(size::PROMPT)
                .style(theme::text_primary)
                .wrapping(text::Wrapping::None),
        );

    // The second line. Mono only for machine text: the command a terminal
    // will run, a device address.
    let (second, mono) = match t {
        Target::Terminal(ask) => (purpose(ask), true),
        Target::Bluetooth { .. } => (t.subject(), true),
        _ => (t.subject(), false),
    };
    if let Some((lead, what)) = second {
        let what = text(what)
            .style(theme::text_secondary)
            .wrapping(text::Wrapping::None);
        let what = if mono {
            what.font(font::DATA).size(size::MONO)
        } else {
            what.font(font::UI_MEDIUM).size(size::BODY)
        };
        col = col.push(Space::new().height(secret::TITLE_GAP)).push(
            Row::new()
                .push(text(lead).size(size::BODY).style(theme::text_secondary))
                .push(what)
                .spacing(space::KEY_GAP)
                .align_y(Alignment::Center),
        );
    }

    if let Target::Terminal(ask) = t {
        let chip = if ask.verified {
            provenance_chip("\u{2713}", &provenance(ask), false, app.hover)
        } else {
            provenance_chip("", "Not from Apotheosis", true, app.hover)
        };
        col = col.push(Space::new().height(secret::GROUP_GAP)).push(
            row![
                chip,
                disclosure("Details", app.unfold.value(), Message::Details, app.hover)
            ]
            .spacing(space::CHIP_GAP)
            .align_y(Alignment::Center),
        );
        // Open, the window refits at once and the panel unfolds into the
        // room (height and opacity); folding plays before the room goes.
        let open = app.unfold.value();
        if app.details || open > 0.0 {
            col = col.push(Space::new().height(secret::DISC_GAP)).push(Reveal::new(
                Fade::new(details(app, ask), open, 0.0, wash(app.blur)),
                open,
            ));
        }
    }

    if let Some(code) = t.shown() {
        // The number to compare, or the code to type: read, not edited, at
        // magnitude scale where the field would be. The code to type is the
        // live yellow (that window commits nothing).
        col = col.push(Space::new().height(secret::FIELD_GAP)).push(
            text(code)
                .font(font::DATA_MEDIUM)
                .size(size::BIG_NUMBER)
                .style(if t.answers() {
                    theme::text_primary
                } else {
                    theme::text_accent
                }),
        );
    } else if t.takes_input() {
        let live = !app.pending && !app.saved;
        let mut input = text_input(hint(app), app.typed.as_str()).id(FIELD);
        if live {
            input = input.on_input(Message::Typed).on_submit(Message::Submit);
        }
        // Each typed character shows as one `*` (ADR 0053 amendment).
        let stars = starred_input(input, app.typed.as_str().chars().count(), live);
        let state = match (&app.problem, live) {
            (Some(_), true) => FieldState::Refused,
            (None, true) => FieldState::Taking,
            (_, false) => FieldState::Resting,
        };
        col = col
            .push(Space::new().height(secret::FIELD_GAP))
            .push(secret_field_pane(state, MARKER, stars, None));
    }

    // A bare yes/no has nothing between its sentence and the pills: give it
    // the field's air so the pills do not crowd the address.
    let bare = t.shown().is_none() && !t.takes_input() && !matches!(t, Target::Terminal(_));
    col.push(Space::new().height(if bare {
        secret::FIELD_GAP
    } else {
        secret::GROUP_GAP
    }))
    .push(actions(app))
    .into()
}

/// Escape cancels. The field captures Escape to unfocus itself, so it is read
/// regardless of capture status; nothing else about a key is looked at, and
/// nothing about one is logged.
pub fn subscription(app: &App) -> Subscription<Message> {
    let moving = app.entrance.animating() || app.unfold.animating() || app.unfold_more.animating();
    Subscription::batch([
        subscription_inner(app),
        // Keep the palette on the compositor's theme (ADR 0084).
        ec_ui::ipc::theme_subscription(|| Message::ThemeChanged),
        ec_ui::glass::xdg_window().map(Message::GlassWindow),
        if moving {
            iced::window::frames().map(Message::Frame)
        } else {
            Subscription::none()
        },
    ])
}

fn subscription_inner(_app: &App) -> Subscription<Message> {
    iced::event::listen_with(|event, _status, _window| match event {
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            ..
        }) => Some(Message::Cancel),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn program_text_loses_escapes_controls_and_bidi_and_is_capped() {
        assert_eq!(crate::clean("[sudo] pw:\u{1b}[31m ", 80), "[sudo] pw:");
        assert_eq!(
            crate::clean("a\u{1b}]0;title\u{7}b\u{1b}]2;x\u{1b}\\c", 80),
            "abc"
        );
        assert_eq!(crate::clean("rm \u{202E}fdp.exe\n\tnow", 80), "rm fdp.exe now");
        assert_eq!(crate::clean("abcdef", 4), "abc…");
        assert_eq!(command_line(&v(&["ssh", "a b", ""]), 80), "ssh 'a b' ''");
    }

    #[test]
    fn the_purpose_is_the_command_sudo_runs() {
        assert_eq!(
            command_after_program(&v(&["sudo", "-A", "pacman", "-Syu"])),
            v(&["pacman", "-Syu"])
        );
        assert_eq!(
            command_after_program(&v(&["/usr/bin/sudo", "-u", "bob", "-A", "id"])),
            v(&["id"])
        );
        assert_eq!(
            command_after_program(&v(&["sudo", "-A", "--", "-weird"])),
            v(&["-weird"])
        );
        assert_eq!(command_after_program(&v(&["git", "push"])), v(&["push"]));
        assert!(command_after_program(&[]).is_empty());
    }
}
