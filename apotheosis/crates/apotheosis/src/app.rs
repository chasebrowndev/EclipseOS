// SPDX-License-Identifier: AGPL-3.0-only
//! The main thread (T-00 §4.2): the calloop loop owning the Wayland connection, the mux
//! session (T-13), key encoding, bind dispatch (T-11) and every PTY write. Each pane has its
//! own PTY and io thread; one render thread draws the window. Main never takes a pane's term
//! lock on the input path and never blocks on GPU, disk or a PTY write. Nothing typed, pasted
//! or printed is ever logged.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use apoth_action::key::Mode as BindMode;
use apoth_action::{
    Action, Cause, Event, EventKind, FieldValue, MAX_MACRO_DEPTH, PlaceholderValues,
};
use apoth_font::config::Style;
use apoth_font::discovery::{FontIndex, default_cache_file, font_dirs};
use apoth_font::system::{EmbeddedFont, FontSystem};
use apoth_input::backend::{Components, KeyLookup, KeyboardBackend};
use apoth_input::backend_kbvm::KbvmBackend;
use apoth_input::modes::bit;
use apoth_input::{
    InputModes, Key, KeyKind, Mods, MouseButton, Named, encode_focus, encode_key, encode_text,
};
use apoth_mux::{
    Dir as MuxDir, FocusMove, PaneSpec as MuxPaneSpec, Session, SplitSize, TabId, TabTarget,
    WindowId as MuxWin,
};
use apoth_pty::{
    CloseTimings, Enqueue, EnvSpec, ExitStatus, Pty, PtySize, SpawnConfig, build_env, choose_term,
    terminfo_dirs,
};
use apoth_term::Term;
use apoth_ui::frame::TabPosition;
use apoth_ui::palette::{Entry, PaletteSpec};
use apoth_ui::pane::FrameMode;
use apoth_ui::searchbar::{Chip, ChipSpec, SearchSpec, SearchTarget};
use apoth_ui::tabs::{Badge, TabSpec};
use apoth_ui::text::TextRun;
use apoth_ui::tokens::size;
use apoth_ui::tooltip::TooltipSpec;
use apoth_ui::{Point, UiState};
use apoth_wl::mime::Want;
use apoth_wl::transfer::TransferDirection;
use apoth_wl::window::Rect;
use apoth_wl::{
    ConfigureEvent, DndEvent, DropContent, ImeEvent, InputEvent, InputKind, KeymapFormat, SeatId,
    SelectionKind, TransferDone, TransferStatus, WindowId, WindowSpec, WlConfig, WlEvent, WlHandle,
};
use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken};

use rustix::io::Errno;

mod a11y;
mod automation;
mod serve;

pub(crate) use a11y::A11yMsg;

use crate::binds::{self, KeyState, Keymap, NameBuf, Resolved};
use crate::hook::{DumpHook, MuxDump};
use crate::layout::{self, PanePx, WinLayout, WinMetrics};
use crate::mousectl::{Mouse, Out};
use crate::nav::{self, Nav, PendingConsent, PendingOpen, SearchUi};
use crate::palette::{self as pal, Palette, Pick, TabItem};
use crate::pane::{
    IoArgs, IoThread, MainMsg, PaneEvents, PaneKey, PaneShared, RenderMsg, ResizeReq,
    WAKE_GRAPHICS_FILE, WAKE_RESIZE, WAKE_STOP, now_ns, spawn_io,
};
use crate::paste::{self, Origin};
use crate::pointer::Geometry;
use crate::reload::{DEBOUNCE, POLL_PERIOD, WatchMode, Watcher};
use crate::render::{
    ChromeModel, PaneView, RenderArgs, RenderSettings, RenderUpdate, TermDefaults, WindowLayout,
    pad_px,
};
use crate::search::{Case, Query};
use crate::security::{self, Answer, Class, Decision};
use crate::settings::{ErrorPos, Settings, Startup, error_positions, load_input, resolve_shell};
use crate::ui::{Overlay, PALETTE_ID, STRIP_ID, bar_inset_px, bars_for, pane_widget, tab_widget};
use crate::view::{Copied, CopyTarget, NavMsg, Preedit, ScrollBy, ViewCmd, ViewSettings};
use crate::workers::Pool;
use apoth_config::fs::RealFs;
use apoth_config::reload::{Applied, Outcome, Reloader, Subsystem};
use apoth_config::types::Reload;
use apoth_ui::WidgetId;

/// Resize coalescing (apoth-pty leaves it to the caller): 25 ms idle.
const RESIZE_IDLE: Duration = Duration::from_millis(25);
/// After the child exits, wait this long for the last output before quitting.
/// Selection autoscroll step while a drag is held past the top or bottom edge.
const AUTOSCROLL_PERIOD: Duration = Duration::from_millis(50);
const EXIT_GRACE: Duration = Duration::from_millis(250);
#[cfg(test)]
const MAX_COLS: usize = layout::MAX_COLS as usize;
#[cfg(test)]
const MAX_ROWS: usize = layout::MAX_ROWS as usize;

/// The last-resort font (T-06 names JetBrains Mono / Instrument Sans embeds; they are not in
/// the tree yet, so the font crate's OFL test fixture stands in).
const EMBEDDED_FONT: &[u8] = include_bytes!("../../apoth-font/tests/fixtures/FiraMono-Medium.ttf");

const PREFERRED_MONO: &[&str] = &[
    "JetBrains Mono",
    "JetBrainsMono Nerd Font Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Fira Mono",
    "Hack",
    "Source Code Pro",
];

/// Pick a monospace family from `(family, monospace, italic)` rows: the first preferred name
/// present, else the first upright monospace family.
pub(crate) fn choose_family(rows: &[(String, bool, bool)]) -> Option<String> {
    for want in PREFERRED_MONO {
        if let Some((f, _, _)) = rows
            .iter()
            .find(|(f, mono, it)| *mono && !*it && f.eq_ignore_ascii_case(want))
        {
            return Some(f.clone());
        }
    }
    rows.iter()
        .find(|(_, mono, it)| *mono && !*it)
        .map(|(f, _, _)| f.clone())
}

/// Grid size for a buffer: whole cells inside the padding (the single-pane rule `layout`
/// generalises; kept as its test oracle).
#[cfg(test)]
pub(crate) fn grid_for(buffer: (u32, u32), pad: (u32, u32), cell: (u32, u32)) -> (usize, usize) {
    let cw = cell.0.max(1);
    let ch = cell.1.max(1);
    let cols = (buffer.0.saturating_sub(2 * pad.0) / cw) as usize;
    let rows = (buffer.1.saturating_sub(2 * pad.1) / ch) as usize;
    (rows.clamp(1, MAX_ROWS), cols.clamp(1, MAX_COLS))
}

/// The app id to set: the `--app-id` request under the T-09 §4 / T-17 §7 rule.
pub(crate) fn resolve_app_id(requested: Option<&str>, on_abyss: bool) -> String {
    let c = apoth_wl::window::resolve_app_id(requested, on_abyss);
    if c.rejected {
        tracing::warn!("--app-id ignored: on Abyss it must start with org.eclipseos.Apotheosis");
    }
    c.id
}

/// The regular family chosen when the configuration names none, so a font reload keeps it.
type AutoFamily = Option<String>;

fn build_fonts(
    s: &Settings,
    scale: f64,
) -> Result<(FontSystem, apoth_font::metrics::CellMetrics, AutoFamily), String> {
    let index = if cfg!(debug_assertions) && std::env::var_os("APOTH_TEST_EMBEDDED_FONT").is_some()
    {
        FontIndex::default()
    } else {
        let dirs = font_dirs(&s.font.directories);
        match default_cache_file() {
            Some(f) => FontIndex::load_or_build(&dirs, &f).0,
            None => FontIndex::build(&dirs, None).0,
        }
    };
    let mut font = s.font.clone();
    let mut auto = None;
    if !s.family_named {
        let rows: Vec<(String, bool, bool)> = index
            .files
            .iter()
            .flat_map(|f| f.faces.iter())
            .map(|f| (f.family.clone(), f.monospace, f.italic))
            .collect();
        if let Some(f) = choose_family(&rows) {
            font.styles[Style::Regular.index()].family = Some(f.clone());
            auto = Some(f);
        }
    }
    let embedded = EmbeddedFont {
        name: "FiraMono-Medium.ttf".into(),
        data: Arc::from(EMBEDDED_FONT),
    };
    let mut fs = FontSystem::new(index, vec![embedded], font);
    let px = fs.config().pixel_size(scale as f32);
    let m = fs.cell_metrics(px).ok_or("no usable font")?;
    Ok((fs, m, auto))
}

/// Longest pane title kept for the tab strip and the window title (T-18 §5).
const MAX_TITLE: usize = 256;

/// A pane: its process, PTY and io thread (T-13 §2: one PTY and io thread per pane).
struct Pane {
    shared: Arc<PaneShared>,
    defaults: TermDefaults,
    pty: Option<Pty>,
    io: Option<IoThread>,
    /// The size the term and PTY currently have (rows, cols).
    dims: (usize, usize),
    pending: Option<(usize, usize)>,
    /// A resize the io thread has been asked for and has not confirmed.
    inflight: Option<(usize, usize)>,
    pollout: Option<RegistrationToken>,
    grace: Option<RegistrationToken>,
    /// Top-left of its grid in the window, physical pixels (meaningful while shown).
    origin: (i32, i32),
    /// The title the program set (OSC 0/2): drawn on the tab, never logged.
    title: String,
    /// The working directory the program reported (OSC 7), when local.
    cwd: Option<PathBuf>,
    /// How the child ended, once reaped.
    exit: Option<ExitStatus>,
    eof: bool,
}

/// A key whose press a bind or the interface took: its repeats and release are taken too, and
/// run the bind's actions when the bind asks for that (T-11 §3).
struct Consumed {
    key: u32,
    on_repeat: Option<Vec<Action>>,
    on_release: Option<Vec<Action>>,
}

impl Consumed {
    fn plain(key: u32) -> Consumed {
        Consumed {
            key,
            on_repeat: None,
            on_release: None,
        }
    }
}

/// Where a pointer position falls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    /// Palette row `n` of the rows in view.
    PaletteRow(usize),
    /// Inside the palette sheet, on no row.
    PaletteSheet,
    /// Outside the palette while it is open.
    PaletteOutside,
    /// Tab `n` of the window, its close button, the new-tab button, or the bare strip.
    Tab(usize),
    TabClose(usize),
    NewTab,
    Strip,
    /// Over a pane's grid.
    Pane(PaneKey),
    /// A button of the overlay bar or the rule sheet.
    BarButton(usize),
    /// The bar or sheet itself, on no button; and the window around a modal sheet.
    BarBody,
    SheetOutside,
    /// A control of the search sheet, or the sheet on none.
    Search(SearchTarget),
    SearchSheet,
    Nothing,
}

/// What the chrome shows about the panes beyond their geometry.
#[derive(Debug, Default)]
struct Status {
    /// OSC 9;4 per pane: state (1 value, 2 error, 3 indeterminate, 4 paused) and 0..=100.
    progress: std::collections::HashMap<PaneKey, (u8, u8)>,
    /// Tabs whose pane rang the bell while the tab was out of sight.
    bell_tabs: std::collections::HashSet<TabId>,
    /// The pane the visual bell is lighting, and the timer that puts it out.
    bell_flash: Option<(PaneKey, RegistrationToken)>,
}

/// How long the visual bell's rim shows.
const BELL_FLASH: Duration = Duration::from_millis(180);

pub(crate) struct App {
    settings: Settings,
    command: Option<Vec<String>>,
    /// `--cwd` and `--title` of the first pane.
    first: FirstPane,
    handle: WlHandle,
    lh: LoopHandle<'static, App>,
    signal: LoopSignal,
    win: WindowId,
    to_main: calloop::channel::Sender<MainMsg>,
    kb: KbvmBackend,
    startup: Option<ConfigureEvent>,
    exit_code: i32,
    app_id: String,
    // ---- mux (T-13)
    session: Session<()>,
    /// The window's id in the session (0 until the first pane starts).
    mux_win: MuxWin,
    panes: BTreeMap<PaneKey, Pane>,
    /// The window's first pane: it owns the unsuffixed test dumps.
    first_pane: PaneKey,
    /// Pane close threads still running; the process exits once none is left and no pane is.
    closing: usize,
    /// Panes main closed (their exit status does not decide the exit code).
    forced: Vec<PaneKey>,
    started: bool,
    render_tx: Option<mpsc::Sender<RenderMsg>>,
    render: Option<JoinHandle<()>>,
    /// Cell size in physical pixels.
    cell: (u32, u32),
    /// The current window geometry and the shown tab's panes.
    geo: Option<WinLayout>,
    placed: Vec<PanePx>,
    resize_timer: Option<RegistrationToken>,
    /// The pane that last had focus (focus in/out reports follow it).
    focus_pane: Option<PaneKey>,
    win_focused: bool,
    // ---- binds and palette (T-11, T-14 §9)
    keymap: Keymap,
    keys: KeyState,
    palette: Option<Palette>,
    ui_state: UiState,
    mux_dump: Option<MuxDump>,
    // ---- hot reload (T-10 §4)
    /// The live configuration and the last diagnostics (T-10 §4); the baseline of every reload.
    reloader: Reloader,
    launch: Vec<(String, String)>,
    workers: Pool,
    watcher: Option<Watcher>,
    watch_mode: WatchMode,
    debounce: Option<RegistrationToken>,
    loading: bool,
    reload_queued: bool,
    /// Where the last rejected load failed (empty when the config is good).
    config_errors: Vec<ErrorPos>,
    /// Restart-class keys changed since start: the notice shows until dismissed.
    restart_keys: Vec<&'static str>,
    /// The overlay bar currently shown, with when it appeared.
    bar: Option<Overlay>,
    /// The user dismissed the bar for the current set of errors / keys.
    bar_dismissed: bool,
    /// Interface-only status per pane/tab (OSC 9;4 progress, bell badges, the visual bell).
    status: Status,
    /// The overlay control a pointer press went down on (acts on release over it).
    overlay_down: Option<Spot>,
    /// The compositor's blur path for the window (decides the auto opacity).
    blur: Option<apoth_wl::BlurPath>,
    /// The Abyss theme's terminal colours while Abyss is detected (T-17 §1, §2).
    abyss_theme: Option<apoth_eclipse::TermColors>,
    auto_family: AutoFamily,
    /// The focused pane's terminal-set title, to re-render the title template on reload.
    term_title: String,
    // ---- selection, clipboard, pointer (T-05 §5-§6, T-09 §6)
    mouse: Mouse,
    /// Held modifiers from the seat's raw masks (depressed and latched; locks excluded).
    held: Mods,
    /// The left button is down (the held button of a motion event).
    left_down: bool,
    /// Last pointer position, surface-logical.
    pointer: Option<(f64, f64)>,
    /// The pane a held button went down in: motion and release go there.
    capture: Option<PaneKey>,
    /// The pane the last selection gesture ran in.
    mouse_pane: Option<PaneKey>,
    /// The seat the last input came from (selections and IME are per seat).
    seat: Option<SeatId>,
    consumed: Vec<Consumed>,
    /// A scroll left the view away from the bottom (typing and pasting snap it back).
    scrolled: bool,
    /// Receive transfers started by a drop, by id (their done event has kind `Clipboard`).
    drop_ids: Vec<u64>,
    /// A paste waiting for the user's answer on the confirm bar.
    paste: Option<PendingPaste>,
    /// text-input-v3 is enabled on the seat.
    ime_on: bool,
    /// The focused pane's cursor cell (row, col), kept whether or not the text input is on.
    cursor_cell: (usize, usize),
    /// The autoscroll timer; present only while a drag is held past an edge.
    autoscroll: Option<RegistrationToken>,
    // ---- control socket (T-12) and accessibility (T-20)
    ctl: Option<serve::Ctl>,
    /// This host's name (OSC 7 URIs naming it are local).
    hostname: String,
    a11y: a11y::A11yState,
    // ---- navigation and consent (T-14 §5-§8, T-18 §3-§4)
    nav: Nav,
    // ---- rules, variables, notifications, profiles (T-11 §5-§9, T-17 §6)
    auto: automation::Auto,
}

/// A paste held back by the guard (T-18 §4). The content is held until answered, never logged.
struct PendingPaste {
    bytes: Vec<u8>,
    stats: paste::Stats,
    /// The pane it goes to.
    pane: PaneKey,
}

/// Command-line choices for the first pane.
#[derive(Debug, Default)]
pub(crate) struct FirstPane {
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) title: Option<String>,
}

/// Run to completion; the process exit code.
pub(crate) fn run(
    startup: Startup,
    command: Option<Vec<String>>,
    app_id: Option<String>,
    first: FirstPane,
) -> Result<i32, String> {
    let Startup {
        settings,
        reloader,
        files,
        launch,
        errors,
    } = startup;
    let on_abyss =
        apoth_wl::window::detect_abyss(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref());
    let app_id = resolve_app_id(app_id.as_deref(), on_abyss);
    let (source, handle) = apoth_wl::connect(WlConfig {
        app_id: Some(app_id.clone()),
        repeat_rate: settings.repeat_rate,
        repeat_delay_ms: settings.repeat_delay_ms,
        ..WlConfig::default()
    })
    .map_err(|e| e.to_string())?;
    let mut event_loop: EventLoop<'static, App> =
        EventLoop::try_new().map_err(|e| e.to_string())?;
    let lh = event_loop.handle();
    lh.insert_source(source, |ev, (), app: &mut App| app.on_wl(ev))
        .map_err(|e| e.error.to_string())?;
    let (tx, rx) = calloop::channel::channel::<MainMsg>();
    lh.insert_source(rx, |ev, (), app: &mut App| {
        if let calloop::channel::Event::Msg(m) = ev {
            app.on_msg(m);
        }
    })
    .map_err(|e| e.error.to_string())?;
    let win = handle.create_window(WindowSpec {
        title: settings.window_title(""),
        app_id: Some(app_id.clone()),
        decorations: settings.decorations,
        ..WindowSpec::default()
    });
    handle.flush();
    let keymap = Keymap::from_config(reloader.config());
    let mut app = App {
        settings,
        command,
        first,
        handle,
        lh,
        signal: event_loop.get_signal(),
        win,
        to_main: tx,
        kb: KbvmBackend::new(),
        startup: None,
        exit_code: 0,
        app_id,
        session: Session::new("default"),
        mux_win: 0,
        panes: BTreeMap::new(),
        first_pane: 0,
        closing: 0,
        forced: Vec::new(),
        started: false,
        render_tx: None,
        render: None,
        cell: (1, 1),
        geo: None,
        placed: Vec::new(),
        resize_timer: None,
        focus_pane: None,
        win_focused: false,
        keymap,
        keys: KeyState::default(),
        palette: None,
        ui_state: UiState::default(),
        mux_dump: MuxDump::from_env(),
        reloader,
        launch,
        workers: Pool::auto(),
        watcher: None,
        watch_mode: WatchMode::Inotify,
        debounce: None,
        loading: false,
        reload_queued: false,
        config_errors: Vec::new(),
        restart_keys: Vec::new(),
        bar: None,
        bar_dismissed: false,
        status: Status::default(),
        overlay_down: None,
        blur: None,
        abyss_theme: None,
        auto_family: None,
        term_title: String::new(),
        mouse: Mouse::new(),
        held: Mods::NONE,
        left_down: false,
        pointer: None,
        capture: None,
        mouse_pane: None,
        seat: None,
        consumed: Vec::new(),
        scrolled: false,
        drop_ids: Vec::new(),
        paste: None,
        ime_on: false,
        cursor_cell: (0, 0),
        autoscroll: None,
        ctl: None,
        hostname: crate::ctl::osc7::hostname(),
        a11y: a11y::A11yState::default(),
        nav: Nav::default(),
        auto: automation::Auto::default(),
    };
    app.start_ctl();
    app.start_a11y();
    app.start_abyss();
    if app.keymap.dropped > 0 {
        tracing::warn!(binds = app.keymap.dropped, "some binds were not understood");
    }
    app.start_watch(&files);
    let mut errors = errors;
    errors.extend(error_positions(&app.recompile_rules()));
    app.seed_consent(automation::load_consent());
    app.set_config_errors(errors);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.started {
        event_loop
            .dispatch(Some(Duration::from_millis(100)), &mut app)
            .map_err(|e| e.to_string())?;
        if let Some(c) = app.startup.take() {
            app.start_window(&c)?;
        }
        if Instant::now() > deadline {
            return Err("the compositor never configured the window".into());
        }
    }
    app.emit_startup();
    event_loop
        .run(None, &mut app, |_| {})
        .map_err(|e| e.to_string())?;
    app.shutdown();
    Ok(app.exit_code)
}

/// The held (non-lock) modifiers under the keymap's current components, resolved by the
/// keyboard backend from the keymap's modifier names (T-05 §1, INPUT-68): Super, Hyper and Meta
/// are virtual modifiers and never read from a fixed real-modifier bit. A key whose type
/// consumes no modifier is probed so nothing is dropped from the reported set.
fn held_of(kb: &impl KeyboardBackend) -> Mods {
    // Escape, Return, BackSpace, F1 (evdev codes): one-level keys in every sane keymap.
    for code in [1u32, 28, 14, 59] {
        if let Some(l) = kb.lookup(code) {
            return l.mods.without_locks();
        }
    }
    Mods::NONE
}

/// The bytes an IME commit sends to the shell. A pending paste confirm owns the keyboard
/// (T-08 §5), so a commit then is dropped, exactly like a typed key. Commit text is user
/// content: encoded (kitty flag 16 aware) and never logged; control characters are dropped.
fn commit_bytes(paste_pending: bool, text: &str, modes: apoth_input::InputModes) -> Vec<u8> {
    let mut out = Vec::new();
    if !paste_pending {
        encode_text(text, modes, &mut out);
    }
    out
}

/// The paste confirm bar's answer to a key: 0 = Paste, 1 = Cancel (Escape too); `None` while the
/// input guard runs, for Return, Space and any unknown key (the paste stays pending).
pub(crate) fn paste_answer(
    spec: &apoth_ui::bar::BarSpec,
    age_ms: u64,
    key: apoth_input::Key,
) -> Option<usize> {
    let input = match key {
        apoth_input::Key::Char(c) => apoth_ui::bar::KeyInput::Char(c),
        apoth_input::Key::Named(apoth_input::Named::Escape) => apoth_ui::bar::KeyInput::Escape,
        apoth_input::Key::Named(apoth_input::Named::Enter) => apoth_ui::bar::KeyInput::Return,
        _ => apoth_ui::bar::KeyInput::Other,
    };
    apoth_ui::bar::BarState { shown_at_ms: 0 }.answer_key(spec, age_ms, input)
}

/// Widget id of the floating search bar / link tooltip.
const SEARCH_ID: apoth_ui::WidgetId = apoth_ui::WidgetId(0x5345_4152_4300);

/// The link-confirm bar (T-14 §5): the full URI as program text, Open or Cancel.
fn open_bar(id: apoth_ui::WidgetId, uri: &str) -> apoth_ui::bar::BarSpec {
    use apoth_ui::bar::{BarKind, BarSpec, ButtonSpec, Key as BarKey};
    BarSpec {
        id,
        kind: BarKind::Consent,
        headline: TextRun::interface("Open this link?", size::BODY).strong(),
        detail: Some(TextRun::program(uri, size::BODY_SMALL)),
        buttons: vec![
            ButtonSpec {
                label: "Open".into(),
                key: BarKey::Char('o'),
                primary: false,
            },
            ButtonSpec {
                label: "Cancel".into(),
                key: BarKey::Char('c'),
                primary: false,
            },
        ],
        escape: Some(1),
        body: None,
    }
}

/// Action ids [`App::run_action`] handles (sorted). The palette lists only these and the
/// socket answers `unsupported` for the rest; `dispatch` asserts the two agree in debug
/// builds.
const HANDLED: &[&str] = &[
    "app.ignore",
    "app.quit",
    "block.jump",
    "clipboard.copy",
    "config.reload",
    "copy-mode.enter",
    "copy-mode.exit",
    "copy-mode.move",
    "copy-mode.toggle",
    "copy-mode.toggle-selection",
    "cursor.jump-char",
    "cursor.jump-repeat",
    "hints.cancel",
    "hints.start",
    "input.paste",
    "input.paste-selection",
    "input.send-bytes",
    "input.send-csi",
    "input.send-esc",
    "input.send-keys",
    "input.send-text",
    "key-table.clear",
    "key-table.pop",
    "key-table.push",
    "layout.next",
    "macro.run",
    "notify",
    "open",
    "palette.close",
    "palette.open",
    "palette.toggle",
    "pane.break-out",
    "pane.close",
    "pane.equalize",
    "pane.focus",
    "pane.focus-last",
    "pane.focus-next",
    "pane.focus-prev",
    "pane.move",
    "pane.resize",
    "pane.split",
    "pane.toggle-zoom",
    "scroll.bottom",
    "scroll.half-page-down",
    "scroll.half-page-up",
    "scroll.line-down",
    "scroll.line-up",
    "scroll.page-down",
    "scroll.page-up",
    "scroll.top",
    "search.cancel",
    "search.next",
    "search.previous",
    "search.set",
    "search.start",
    "search.toggle-case",
    "search.toggle-regex",
    "selection.clear",
    "selection.copy",
    "selection.select-all",
    "tab.close",
    "tab.close-others",
    "tab.last",
    "tab.move",
    "tab.new",
    "tab.next",
    "tab.prev",
    "tab.rename",
    "tab.select",
    "var.set",
    "var.unset",
    "window.close",
];

/// Does the dispatcher handle action `id` yet?
pub(crate) fn handles(id: &str) -> bool {
    HANDLED.binary_search(&id).is_ok()
}

fn exit_code_of(s: ExitStatus) -> i32 {
    match s {
        ExitStatus::Code(c) => c,
        ExitStatus::Signal(n) => 128 + n,
    }
}

/// The bind grammar's directions to the mux's.
fn mux_dir(d: apoth_action::enums::Dir) -> MuxDir {
    use apoth_action::enums::Dir;
    match d {
        Dir::Left => MuxDir::Left,
        Dir::Right => MuxDir::Right,
        Dir::Up => MuxDir::Up,
        Dir::Down => MuxDir::Down,
    }
}

fn split_dir(d: apoth_action::enums::SplitDir) -> MuxDir {
    use apoth_action::enums::SplitDir;
    match d {
        SplitDir::Left => MuxDir::Left,
        SplitDir::Right => MuxDir::Right,
        SplitDir::Up => MuxDir::Up,
        SplitDir::Down => MuxDir::Down,
    }
}

/// `mode=` conditions from a pane's mirror (T-11 §3).
fn mode_holds(
    m: BindMode,
    modes: InputModes,
    has_selection: bool,
    copy: bool,
    searching: bool,
) -> bool {
    match m {
        BindMode::AltScreen => modes.has(bit::ALT_SCREEN),
        BindMode::AppCursor => modes.has(bit::DECCKM),
        BindMode::KittyKeyboard => modes.kitty_flags() != 0,
        BindMode::HasSelection => has_selection,
        BindMode::CopyMode => copy,
        BindMode::Searching => searching,
    }
}

/// `performable=#true` binds pass the key through when the action would do nothing: scrolling
/// has nothing to do on the alternate screen (it has no scrollback).
fn performable(actions: &[Action], modes: InputModes) -> bool {
    actions.iter().any(|a| {
        let scroll = a.id().starts_with("scroll.");
        !scroll || !modes.has(bit::ALT_SCREEN)
    })
}

/// Cut `s` to at most `max` bytes on a character boundary.
fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The tab strip is shown for `tabs` tabs under `tabs.show` and `tabs.style`.
fn strip_shown(show: Option<&str>, style: Option<&str>, tabs: usize) -> bool {
    if style == Some("hidden") {
        return false;
    }
    match show {
        Some("always") => true,
        Some("never") => false,
        _ => tabs > 1,
    }
}

impl App {
    fn scale(&self) -> f64 {
        self.handle.scale(self.win).map_or(1.0, |s| s.as_f64())
    }

    /// Physical pixels the overlay bar takes at the top (the grid never sits under it).
    fn bar_px(&self, scale: f64) -> u32 {
        self.bar.as_ref().filter(|o| o.takes_room()).map_or(0, |_| {
            bar_inset_px(scale, self.settings.glass.interface_zoom)
        })
    }

    fn tab_count(&self) -> usize {
        self.session
            .window(self.mux_win)
            .map_or(1, |w| w.tabs.len())
    }

    /// Where the tab strip is, when it is shown (T-08 §4: `tabs.*`).
    fn strip_position(&self, tabs: usize) -> Option<TabPosition> {
        let c = self.reloader.config();
        strip_shown(c.str("tabs.show"), c.str("tabs.style"), tabs).then(|| {
            if c.str("tabs.position") == Some("bottom") {
                TabPosition::Bottom
            } else {
                TabPosition::Top
            }
        })
    }

    fn metrics(&self, buffer: (u32, u32)) -> WinMetrics {
        let scale = self.scale();
        WinMetrics {
            buffer,
            scale,
            zoom: self.settings.glass.interface_zoom,
            pad: (
                pad_px(self.settings.padding.0, scale),
                pad_px(self.settings.padding.1, scale),
            ),
            bar: self.bar_px(scale),
            strip: self.strip_position(self.tab_count()),
            cell: self.cell,
        }
    }

    /// The focused pane of the shown tab.
    fn focused(&self) -> Option<PaneKey> {
        let w = self.session.window(self.mux_win)?;
        self.session.tab(w.active?)?.focused()
    }

    fn active_tab(&self) -> Option<TabId> {
        self.session.window(self.mux_win)?.active
    }

    fn start_window(&mut self, c: &ConfigureEvent) -> Result<(), String> {
        self.blur = self.handle.blur_path(self.win);
        let scale = self.scale();
        let (fonts, metrics, auto_family) = build_fonts(&self.settings, scale)?;
        self.auto_family = auto_family;
        self.cell = (metrics.width as u32, metrics.height as u32);
        if c.size.is_none() {
            let (irows, icols) = self.settings.initial_size;
            let m = self.metrics((0, 0));
            let strip = m
                .strip
                .map_or(0, |_| layout::strip_px(layout::window(&m).ui));
            let w = icols as u32 * self.cell.0 + 2 * m.pad.0;
            let h = irows as u32 * self.cell.1 + 2 * m.pad.1 + m.bar + strip;
            let lw = (f64::from(w) / scale).ceil() as u32;
            let lh = (f64::from(h) / scale).ceil() as u32;
            self.handle.set_window_size(self.win, (lw, lh));
        }
        let (_, buffer) = self
            .handle
            .surface_size(self.win)
            .ok_or("window has no size")?;
        let geo = layout::window(&self.metrics(buffer));
        let spec = MuxPaneSpec {
            cwd: None,
            argv: Vec::new(),
            profile: None,
        };
        let (wid, _, id) = self.session.new_window(geo.size, None, spec, ());
        self.mux_win = wid;
        self.first_pane = id;
        let dims = self
            .dims_of(id)
            .unwrap_or((geo.size.rows as usize, geo.size.cols as usize));
        // The render channel first: the pane's io thread pokes the render thread through it.
        let (render_tx, render_rx) = mpsc::channel();
        self.render_tx = Some(render_tx.clone());
        let command = self.command.take();
        let cwd = self.first.cwd.take();
        let pane = self.spawn_pane(id, dims, command, cwd, Vec::new())?;
        if let Some(t) = self.first.title.take()
            && let Some(tid) = self.session.tab_of(id)
        {
            let _ = self.session.rename_tab(tid, clip(&t, MAX_TITLE));
        }

        let surface = self
            .handle
            .surface_handle(self.win)
            .ok_or("window has no surface")?;
        let pacer = self.handle.pacer(self.win).ok_or("window has no pacer")?;
        let render = crate::render::spawn(RenderArgs {
            surface,
            pacer,
            fonts,
            settings: RenderSettings::from_settings(&self.settings, self.blur),
            scale,
            buffer,
            rx: render_rx,
            poke: render_tx.clone(),
            to_main: self.to_main.clone(),
        })
        .map_err(|e| e.to_string())?;
        self.render = Some(render);
        self.panes.insert(id, pane);
        self.started = true;
        tracing::info!(
            rows = dims.0,
            cols = dims.1,
            scale,
            app_id = %self.app_id,
            workers = self.workers.threads(),
            watch = ?self.watch_mode,
            "window started"
        );
        // A bar raised by the startup load (or before the window existed) reaches the renderer.
        self.send_overlay();
        self.relayout();
        self.send_view_settings(id);
        Ok(())
    }

    /// The grid a pane gets in the current layout of its tab (shown or not).
    fn dims_of(&self, id: PaneKey) -> Option<(usize, usize)> {
        let tid = self.session.tab_of(id)?;
        let tab = self.session.tab(tid)?;
        let size = tab.size();
        layout::panes(&tab.layout(), size, (0, 0), self.cell)
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.dims)
    }

    /// Start a pane's shell (or `argv`) on a PTY with its io thread. `cwd` falls back to
    /// `shell.cwd`. Nothing here is logged but sizes: argv and cwd are user data.
    fn spawn_pane(
        &mut self,
        id: PaneKey,
        (rows, cols): (usize, usize),
        argv: Option<Vec<String>>,
        cwd: Option<PathBuf>,
        env_extra: Vec<(String, String)>,
    ) -> Result<Pane, String> {
        let cell = self.cell;
        let mut tc = self.settings.term.clone();
        tc.rows = rows;
        tc.cols = cols;
        tc.cell_px = cell;
        let term = Term::new(tc);
        let defaults = TermDefaults {
            fg: term.colors().0,
            bg: term.colors().1,
            cursor: term.colors().2,
            ansi: std::array::from_fn(|i| term.palette(i as u8)),
        };
        let shared = Arc::new(PaneShared::new(term));
        // The sequence policy's deny word, local column (T-18 §3); main re-checks remote
        // panes itself before every clipboard decision.
        shared.deny.store(self.deny_word(), Ordering::Release);

        let size = PtySize::from_cells(rows as u16, cols as u16, cell.0, cell.1);
        let (argv, login) = match argv {
            Some(c) if !c.is_empty() => (c.iter().map(OsString::from).collect(), false),
            _ => {
                let mut v = vec![OsString::from(resolve_shell(
                    self.settings.shell.program.as_deref(),
                ))];
                v.extend(self.settings.shell.args.iter().map(OsString::from));
                (v, self.settings.shell.login)
            }
        };
        let term_name = if self.settings.term_name == "xterm-apotheosis" {
            choose_term(false, &terminfo_dirs(&|k| std::env::var_os(k))).to_owned()
        } else {
            self.settings.term_name.clone()
        };
        let mut extra: Vec<(OsString, OsString)> = vec![
            (
                "APOTHEOSIS_VERSION".into(),
                env!("CARGO_PKG_VERSION").into(),
            ),
            ("APOTHEOSIS_PANE".into(), id.to_string().into()),
            ("APOTHEOSIS_WINDOW".into(), self.mux_win.to_string().into()),
        ];
        if let Some(p) = self.ctl_path() {
            extra.push(("APOTHEOSIS_SOCKET".into(), p.as_os_str().to_owned()));
        }
        extra.extend(
            self.settings
                .shell
                .env
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v))),
        );
        extra.extend(
            env_extra
                .into_iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v))),
        );
        let env = build_env(
            std::env::vars_os(),
            &EnvSpec {
                term: &term_name,
                version: env!("CARGO_PKG_VERSION"),
                extra,
            },
        );
        let pty = Pty::spawn(&SpawnConfig {
            argv,
            login,
            cwd: cwd.or_else(|| self.settings.shell.cwd.clone()),
            env,
            size,
            iutf8: true,
        })
        .map_err(|e| format!("cannot start the shell: {e}"))?;
        let master = pty
            .master_fd()
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        let pidfd = pty
            .pidfd()
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        let (wake_rx, wake_tx) =
            rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(|e| e.to_string())?;
        let to_render = self
            .render_tx
            .clone()
            .ok_or("the render channel is not set up")?;
        let first = self.first_pane == id;
        let io = spawn_io(
            IoArgs {
                id,
                shared: Arc::clone(&shared),
                master,
                wake_rx,
                to_main: self.to_main.clone(),
                to_render,
                hook: DumpHook::for_pane(first, id),
                workers: self.workers.spawner(),
            },
            wake_tx,
        )
        .map_err(|e| e.to_string())?;
        self.lh
            .insert_source(
                Generic::new(pidfd, Interest::READ, Mode::Level),
                move |_, _, app: &mut App| {
                    app.on_child_exit(id);
                    Ok(PostAction::Remove)
                },
            )
            .map_err(|e| e.error.to_string())?;
        tracing::debug!(pane = id, rows, cols, "pane started");
        Ok(Pane {
            shared,
            defaults,
            pty: Some(pty),
            io: Some(io),
            dims: (rows, cols),
            pending: None,
            inflight: None,
            pollout: None,
            grace: None,
            origin: (0, 0),
            title: String::new(),
            cwd: None,
            exit: None,
            eof: false,
        })
    }

    /// The working directory a new pane inherits (T-13 §6): the focused pane's OSC 7
    /// directory when it reported a local one, else its foreground process cwd from `/proc`.
    fn inherited_cwd(&self) -> Option<PathBuf> {
        let p = self.panes.get(&self.focused()?)?;
        if let Some(c) = &p.cwd {
            return Some(c.clone());
        }
        p.pty.as_ref()?.foreground_cwd().ok()
    }

    // ------------------------------------------------------------------ layout

    /// Recompute the window geometry, every tab's pane grids, focus, and what the window
    /// shows. Every mux change, resize, scale, padding, bar and strip change lands here.
    fn relayout(&mut self) {
        if !self.started {
            return;
        }
        if let Some(a) = self.active_tab() {
            self.status.bell_tabs.remove(&a);
        }
        let Some((_, buffer)) = self.handle.surface_size(self.win) else {
            return;
        };
        let geo = layout::window(&self.metrics(buffer));
        let _ = self.session.resize_window(self.mux_win, geo.size);
        let Some(w) = self.session.window(self.mux_win) else {
            return;
        };
        let active = w.active;
        let mut wanted: Vec<(PaneKey, (usize, usize))> = Vec::new();
        let mut shown = Vec::new();
        for &tid in &w.tabs {
            let Some(tab) = self.session.tab(tid) else {
                continue;
            };
            let placed = layout::panes(&tab.layout(), tab.size(), geo.origin, self.cell);
            wanted.extend(placed.iter().map(|p| (p.id, p.dims)));
            if Some(tid) == active {
                shown = placed;
            }
        }
        for p in &shown {
            if let Some(pane) = self.panes.get_mut(&p.id) {
                pane.origin = p.origin;
            }
        }
        self.geo = Some(geo);
        self.placed = shown;
        for (id, dims) in wanted {
            self.request_resize(id, dims);
        }
        self.sync_focus();
        self.send_layout();
        self.ime_send_rect();
    }

    /// Tell the render thread what the window shows (panes and chrome) and refresh the test
    /// dump. Cheap: hover changes call only this.
    fn send_layout(&mut self) {
        let Some(geo) = self.geo else { return };
        let focus = self.focused();
        let panes: Vec<PaneView> = self
            .placed
            .iter()
            .filter_map(|p| {
                let pane = self.panes.get(&p.id)?;
                Some(PaneView {
                    id: p.id,
                    shared: Arc::clone(&pane.shared),
                    defaults: pane.defaults.clone(),
                    origin: p.origin,
                    focused: Some(p.id) == focus,
                    first: p.id == self.first_pane,
                })
            })
            .collect();
        let chrome = self.chrome_model(&geo, focus);
        if let Some(tx) = &self.render_tx {
            let _ = tx.send(RenderMsg::Layout(Box::new(WindowLayout { panes, chrome })));
        }
        self.write_mux_dump();
    }

    /// A tab's title: its pinned name, else its focused pane's program title, else "shell".
    fn tab_title(&self, tid: TabId) -> String {
        let Some(tab) = self.session.tab(tid) else {
            return String::new();
        };
        if !tab.name().is_empty() {
            return tab.name().to_owned();
        }
        let pane = tab.focused();
        let title = pane
            .and_then(|p| self.panes.get(&p))
            .map(|p| p.title.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or("shell");
        self.tab_label(pane, title)
    }

    fn tab_specs(&self) -> Vec<TabSpec> {
        let Some(w) = self.session.window(self.mux_win) else {
            return Vec::new();
        };
        w.tabs
            .iter()
            .map(|&tid| {
                let pane = self.session.tab(tid).and_then(|t| t.focused());
                TabSpec {
                    id: tab_widget(tid),
                    title: TextRun::program(&self.tab_title(tid), size::BODY),
                    active: Some(tid) == w.active,
                    badges: self.tab_badges(tid),
                    profile: pane
                        .and_then(|p| self.pane_profile(p))
                        .map(|n| TextRun::program(n, apoth_ui::tokens::size::MICRO)),
                }
            })
            .collect()
    }

    /// A tab's badges: the bell that rang out of sight, and the OSC 9;4 progress of its panes.
    fn tab_badges(&self, tid: TabId) -> Vec<Badge> {
        let mut out = Vec::new();
        if self.status.bell_tabs.contains(&tid) {
            out.push(Badge::Bell);
        }
        let Some(tab) = self.session.tab(tid) else {
            return out;
        };
        // The focused pane's progress wins, else the first pane that reports one.
        let panes = tab.panes();
        let pick = tab
            .focused()
            .filter(|p| self.status.progress.contains_key(p))
            .or_else(|| {
                panes
                    .iter()
                    .copied()
                    .find(|p| self.status.progress.contains_key(p))
            });
        if let Some(&(state, v)) = pick.and_then(|p| self.status.progress.get(&p)) {
            out.push(match state {
                2 => Badge::ProgressError(v),
                3 => Badge::ProgressBusy,
                4 => Badge::ProgressPaused(v),
                _ => Badge::Progress(v),
            });
        }
        out
    }

    /// The palette's rows in view, as the sheet draws them.
    fn palette_spec(&self, area: apoth_ui::Rect) -> Option<PaletteSpec> {
        let p = self.palette.as_ref()?;
        let n = apoth_ui::palette::max_rows(area);
        let (rows, selected) = p.window(n);
        let program = p.mode == pal::Mode::Tabs;
        let entries = rows
            .iter()
            .map(|r| Entry {
                title: if program {
                    TextRun::program(&r.title, size::BODY)
                } else {
                    TextRun::interface(r.title.clone(), size::BODY)
                },
                hint: r
                    .hint
                    .as_ref()
                    .map(|h| TextRun::interface(h.clone(), size::BODY_SMALL)),
            })
            .collect();
        Some(PaletteSpec {
            id: PALETTE_ID,
            query: p.query.clone(),
            placeholder: TextRun::interface(p.mode.placeholder(), size::BODY),
            entries,
            selected,
            total: p.rows.len(),
        })
    }

    fn chrome_model(&self, geo: &WinLayout, focus: Option<PaneKey>) -> ChromeModel {
        let c = self.reloader.config();
        let style = match c.str("tabs.style") {
            Some("flat") => apoth_ui::frame::TabStyle::Flat,
            Some("hidden") => apoth_ui::frame::TabStyle::Hidden,
            _ => apoth_ui::frame::TabStyle::Floating,
        };
        let mode = match c.str("panes.frame") {
            Some("none") => FrameMode::None,
            Some("titles") => FrameMode::Titles,
            _ => FrameMode::Full,
        };
        let dim = c.float("panes.inactive-dim").clamp(0.0, 1.0) as f32;
        // Frames only matter when there is more than one pane to tell apart.
        let frames = if self.placed.len() > 1 && mode != FrameMode::None {
            self.placed
                .iter()
                .map(|p| apoth_ui::pane::PaneSpec {
                    id: pane_widget(p.id),
                    rect: layout::frame_rect(p, self.cell, geo.ui, geo.area),
                    focused: Some(p.id) == focus,
                    title: None,
                    mode,
                    inactive_dim: dim,
                })
                .collect()
        } else {
            Vec::new()
        };
        ChromeModel {
            strip: geo.strip.map(|r| (r, self.tab_specs())),
            tab_style: style,
            frames,
            palette: self.palette_spec(geo.area),
            search: self.search_spec(),
            tooltip: self.tooltip_spec(),
            bell: self.bell_rect(geo),
            ui: self.ui_state.clone(),
        }
    }

    /// The search sheet's content (T-14 §6): the query, the count and the mode chips.
    fn search_spec(&self) -> Option<SearchSpec> {
        let s = self.nav.search.as_ref()?;
        let (count, count_kind) = Nav::search_count(s);
        let chip = |chip, label: &str, on| ChipSpec {
            chip,
            label: label.to_owned(),
            on,
        };
        Some(SearchSpec {
            id: SEARCH_ID,
            query: s.query.text.clone(),
            placeholder: TextRun::interface("Find", size::BODY),
            count,
            count_kind,
            chips: vec![
                match s.query.case {
                    Case::Smart => chip(Chip::Case, "Aa", false),
                    Case::Sensitive => chip(Chip::Case, "Aa", true),
                    Case::Insensitive => chip(Chip::Case, "aa", true),
                },
                chip(Chip::Regex, ".*", s.query.regex),
                chip(Chip::Word, "\\b", s.query.whole_word),
                chip(
                    Chip::Scope,
                    "Screen",
                    s.query.scope == crate::search::Scope::Screen,
                ),
            ],
            focused: s.focused,
            can_step: s.status.count > 0,
        })
    }

    /// The hovered link's URI by the pointer (T-14 §5): program text, so sanitised and capped.
    fn tooltip_spec(&self) -> Option<TooltipSpec> {
        let uri = self.nav.link_tip.as_ref()?;
        let (x, y) = self.pointer?;
        Some(TooltipSpec {
            text: TextRun::program(uri, size::BODY_SMALL),
            anchor: self.ui_point(x, y)?,
        })
    }

    /// The frame of the pane the visual bell is lighting.
    fn bell_rect(&self, geo: &WinLayout) -> Option<apoth_ui::Rect> {
        let (id, _) = self.status.bell_flash.as_ref()?;
        let p = self.placed.iter().find(|p| p.id == *id)?;
        Some(layout::frame_rect(p, self.cell, geo.ui, geo.area))
    }

    /// The test-only mux dump (`APOTH_TEST_MUX`, debug builds): tabs, panes, focus, palette.
    fn write_mux_dump(&mut self) {
        if self.mux_dump.is_none() {
            return;
        }
        let mut s = String::new();
        let focus = self.focused();
        if let Some(w) = self.session.window(self.mux_win) {
            let active = w.active.and_then(|a| w.tabs.iter().position(|&t| t == a));
            let _ = writeln!(s, "tabs {} active {}", w.tabs.len(), active.unwrap_or(0));
            for (i, &tid) in w.tabs.iter().enumerate() {
                let Some(tab) = self.session.tab(tid) else {
                    continue;
                };
                let ids: Vec<String> = tab.panes().iter().map(u64::to_string).collect();
                let _ = writeln!(
                    s,
                    "tab {i} panes {} focused {}",
                    ids.join(","),
                    tab.focused().unwrap_or(0)
                );
            }
        }
        for p in &self.placed {
            let dims = self.panes.get(&p.id).map_or((0, 0), |x| x.dims);
            let _ = writeln!(
                s,
                "pane {} rows {} cols {} at {},{}",
                p.id, dims.0, dims.1, p.origin.0, p.origin.1
            );
        }
        let _ = writeln!(s, "focus {}", focus.unwrap_or(0));
        match &self.palette {
            Some(p) => {
                let _ = writeln!(
                    s,
                    "palette open mode {:?} rows {} query {}",
                    p.mode,
                    p.rows.len(),
                    p.query
                );
            }
            None => s.push_str("palette closed\n"),
        }
        let tables = self.keys.active(&self.keymap).join(",");
        let _ = writeln!(s, "tables {tables}");
        if let Some(d) = self.mux_dump.as_mut() {
            d.write(s);
        }
    }

    /// Focus moved between panes (or the window gained or lost it): update the panes' focus
    /// flags and send focus out / in reports (mode 1004) to the panes involved.
    fn sync_focus(&mut self) {
        let now = self.focused();
        if now == self.focus_pane {
            return;
        }
        let old = std::mem::replace(&mut self.focus_pane, now);
        if let Some(o) = old {
            self.end_nav(o);
            self.report_focus(o, false);
            if self.panes.contains_key(&o) {
                self.ctl_emit(Event::new(EventKind::FocusOut, Cause::User), Some(o));
            }
        }
        if now.is_some() {
            self.ctl_emit(Event::new(EventKind::FocusIn, Cause::User), now);
        }
        if let Some(n) = now {
            if self.win_focused {
                self.report_focus(n, true);
            }
            self.term_title = self
                .panes
                .get(&n)
                .map(|p| p.title.clone())
                .unwrap_or_default();
            let title = self.window_title_now();
            self.handle.set_title(self.win, &title);
            // The key-table stack and a half-typed sequence belong to the old pane's context.
            self.keys.reset();
        }
    }

    fn report_focus(&mut self, id: PaneKey, focused: bool) {
        let Some(p) = self.panes.get(&id) else { return };
        let was = p.shared.focused.swap(focused, Ordering::AcqRel);
        if was == focused {
            return;
        }
        let mut buf = [0u8; 3];
        let n = encode_focus(focused, p.shared.mirror.load(), &mut buf);
        if n > 0 {
            self.send_bytes(id, &buf[..n]);
        }
    }

    // ------------------------------------------------------------------ mux actions

    fn new_tab(&mut self, argv: Option<Vec<String>>, cwd: Option<PathBuf>) {
        let _ = self.new_tab_with(argv, cwd, Vec::new());
    }

    /// A new tab running `argv` (or the shell) with extra environment; its pane id, if it
    /// started.
    fn new_tab_with(
        &mut self,
        argv: Option<Vec<String>>,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
    ) -> Option<PaneKey> {
        let cwd = cwd.or_else(|| self.inherited_cwd());
        let spec = MuxPaneSpec::default();
        let (_, id) = self.session.new_tab(self.mux_win, spec, ()).ok()?;
        self.finish_new_pane(id, argv, cwd, env);
        if !self.panes.contains_key(&id) {
            return None;
        }
        self.ctl_emit(Event::new(EventKind::TabOpened, Cause::User), Some(id));
        Some(id)
    }

    fn split(
        &mut self,
        dir: MuxDir,
        argv: Option<Vec<String>>,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
    ) -> Option<PaneKey> {
        let target = self.focused()?;
        let cwd = cwd.or_else(|| self.inherited_cwd());
        let spec = MuxPaneSpec::default();
        match self
            .session
            .split_pane(target, dir, SplitSize::HALF, spec, ())
        {
            Ok(id) => {
                self.finish_new_pane(id, argv, cwd, env);
                self.panes.contains_key(&id).then_some(id)
            }
            Err(e) => {
                // Too small for another pane (T-13 §3: 2x1 minimum) and the like.
                tracing::debug!(error = %e, "split refused");
                self.handle.ring_bell(Some(self.win));
                None
            }
        }
    }

    /// Resolve a `profile=` for a new pane: its shell, cwd and env under any explicit argv
    /// and cwd. `None` (with a bell) for an unknown profile.
    #[allow(clippy::type_complexity)]
    fn with_profile(
        &mut self,
        profile: Option<&str>,
        argv: Option<Vec<String>>,
        cwd: Option<PathBuf>,
    ) -> Option<(Option<Vec<String>>, Option<PathBuf>, Vec<(String, String)>)> {
        let Some(name) = profile else {
            return Some((argv, cwd, Vec::new()));
        };
        match self.profile_launch(name, argv, cwd) {
            Ok(r) => Some(r),
            Err(()) => {
                tracing::info!("unknown profile");
                self.handle.ring_bell(Some(self.win));
                None
            }
        }
    }

    /// The session has the pane; start its process, or take it back out if that fails.
    fn finish_new_pane(
        &mut self,
        id: PaneKey,
        argv: Option<Vec<String>>,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
    ) {
        // The tab may have changed size (a new tab can show the strip): lay out first.
        let (_, buffer) = self.handle.surface_size(self.win).unwrap_or_default();
        let geo = layout::window(&self.metrics(buffer));
        let _ = self.session.resize_window(self.mux_win, geo.size);
        let dims = self.dims_of(id).unwrap_or((1, 1));
        match self.spawn_pane(id, dims, argv, cwd, env) {
            Ok(p) => {
                self.panes.insert(id, p);
                self.send_view_settings(id);
                self.ctl_emit(Event::new(EventKind::PaneOpened, Cause::User), Some(id));
            }
            Err(why) => {
                tracing::warn!(%why, "cannot start a pane");
                let _ = self.session.close_pane(id);
            }
        }
        self.relayout();
    }

    /// Close a pane at the user's request: SIGHUP escalation runs off the loop (T-18 §7).
    fn close_pane(&mut self, id: PaneKey) {
        if !self.forced.contains(&id) {
            self.forced.push(id);
        }
        self.retire(id);
    }

    /// Take a pane out of the session and the window, and stop its io thread and process on a
    /// close thread. The process exits when the last pane is gone and every close finished.
    fn retire(&mut self, id: PaneKey) {
        let tab = self.session.tab_of(id);
        let code = self.panes.get(&id).and_then(|p| p.exit).map(exit_code_of);
        self.ctl_emit(Event::new(EventKind::PaneClosed, Cause::Child), Some(id));
        self.ctl_pane_gone(id, code);
        let _ = self.session.close_pane(id);
        if let Some(t) = tab
            && self.session.tab(t).is_none()
        {
            let mut ev = Event::new(EventKind::TabClosed, Cause::Child);
            let _ = ev.set("tab", FieldValue::Int(t as i64));
            self.ctl_emit(ev, None);
        }
        self.nav.consents.forget(id);
        self.auto_forget(id);
        if let Some(t) = tab
            && self.session.tab(t).is_none()
        {
            self.auto_forget_tab(t);
        }
        self.nav.osc52_reads.retain(|r| r.1 != id);
        if self.nav.consent.as_ref().is_some_and(|c| c.pane == id) {
            self.nav.consent = None;
            self.refresh_bar();
        }
        if self.capture == Some(id) {
            self.capture = None;
        }
        if self.mouse_pane == Some(id) {
            self.mouse_pane = None;
        }
        if self.paste.as_ref().is_some_and(|p| p.pane == id) {
            self.paste = None;
            self.refresh_bar();
        }
        self.status.progress.remove(&id);
        if let Some(mut p) = self.panes.remove(&id) {
            for t in [p.pollout.take(), p.grace.take()].into_iter().flatten() {
                self.lh.remove(t);
            }
            if p.exit.is_some() && !self.forced.contains(&id) && self.panes.is_empty() {
                // The last pane ended by itself: its status is the process's (T-13 §8).
                self.exit_code = p.exit.map_or(0, exit_code_of).min(255);
            }
            let io = p.io.take();
            let pty = p.pty.take();
            if let Some(io) = &io {
                let _ = rustix::io::write(&io.wake, &[WAKE_STOP]);
            }
            let tx = self.to_main.clone();
            let spawned = std::thread::Builder::new()
                .name("apoth-close".into())
                .spawn(move || {
                    let st =
                        pty.and_then(|mut pty| pty.close(&CloseTimings::default()).ok().flatten());
                    if let Some(io) = io {
                        let _ = io.join.join();
                    }
                    let _ = tx.send(MainMsg::Closed(id, st));
                });
            if spawned.is_ok() {
                self.closing += 1;
            }
        }
        self.relayout();
        self.maybe_quit();
    }

    fn maybe_quit(&mut self) {
        if self.started && self.panes.is_empty() && self.closing == 0 {
            self.quit();
        }
    }

    fn close_tab(&mut self, tid: TabId) {
        let ids = self.session.tab(tid).map(|t| t.panes()).unwrap_or_default();
        for id in ids {
            self.close_pane(id);
        }
    }

    /// Close every pane (window close, `app.quit`).
    fn begin_close(&mut self) {
        if !self.started {
            self.quit();
            return;
        }
        let ids: Vec<PaneKey> = self.panes.keys().copied().collect();
        for id in ids {
            self.close_pane(id);
        }
        self.maybe_quit();
    }

    fn with_tab(&mut self, f: impl FnOnce(&mut apoth_mux::Tab, PaneKey)) {
        let Some(tid) = self.active_tab() else { return };
        let Some(tab) = self.session.tab_mut(tid) else {
            return;
        };
        let Some(focus) = tab.focused() else { return };
        f(tab, focus);
        self.relayout();
    }

    fn focus_tab(&mut self, t: TabTarget) {
        if self.session.focus_tab(self.mux_win, t).is_ok() {
            self.relayout();
        }
    }

    fn focus_pane_at(&mut self, id: PaneKey) {
        if self.focused() == Some(id) {
            return;
        }
        let Some(tid) = self.session.tab_of(id) else {
            return;
        };
        if let Some(tab) = self.session.tab_mut(tid)
            && tab.focus(id).is_ok()
        {
            self.relayout();
        }
    }

    // ------------------------------------------------------------------ wayland

    fn on_wl(&mut self, ev: WlEvent) {
        match ev {
            WlEvent::Configure(c) => {
                if self.started {
                    self.relayout();
                    self.poke();
                } else {
                    self.startup = Some(c);
                }
            }
            WlEvent::ScaleChanged { .. } => {
                self.relayout();
                self.poke();
            }
            WlEvent::BlurPathChanged { path, .. } => {
                self.blur = Some(path);
                self.send_render_update(false);
            }
            WlEvent::CloseRequested(_) => self.begin_close(),
            WlEvent::Frame { .. }
            | WlEvent::Presented(_)
            | WlEvent::PresentationDiscarded { .. } => self.poke(),
            WlEvent::Input(i) => self.on_input(i),
            WlEvent::Send(t) => {
                // A client asked for our selection: the transfer runs on the loop.
                if t.insert(&self.lh, |_: TransferDone, _: &mut App| {})
                    .is_err()
                {
                    tracing::warn!("cannot start a selection send");
                }
            }
            WlEvent::Dnd(DndEvent::Dropped { seat, .. }) => self.start_drop(seat),
            WlEvent::Fatal(f) => {
                tracing::error!(interface = %f.interface, code = f.code, "wayland protocol error");
                self.exit_code = 1;
                self.quit();
            }
            _ => {}
        }
    }

    fn poke(&self) {
        if let Some(tx) = &self.render_tx {
            let _ = tx.send(RenderMsg::Poke);
        }
    }

    fn quit(&self) {
        self.signal.stop();
        self.signal.wakeup();
    }

    fn on_input(&mut self, i: InputEvent) {
        self.seat = Some(i.seat);
        match i.kind {
            InputKind::Keymap(km) => {
                if km.format == KeymapFormat::XkbV1
                    && let Err(e) = self.kb.load_keymap_bytes(&km.bytes)
                {
                    tracing::warn!(reason = e.0, "keymap rejected");
                }
            }
            InputKind::Modifiers {
                depressed,
                latched,
                locked,
                group,
            } => {
                self.kb.update_modifiers(Components {
                    depressed,
                    latched,
                    locked,
                    group,
                });
                self.held = held_of(&self.kb);
            }
            InputKind::ModifiersReset => {
                self.held = Mods::NONE;
                self.kb.update_modifiers(Components::default());
            }
            InputKind::PointerEnter { x, y, .. } | InputKind::PointerMotion { x, y } => {
                self.on_pointer_motion(x, y);
            }
            InputKind::PointerLeave { .. } => {
                self.pointer = None;
                self.mouse.left();
                self.set_hover(None);
            }
            InputKind::PointerButton {
                button, pressed, ..
            } => self.on_pointer_button(button, pressed),
            InputKind::Scroll(f) => self.on_scroll(f.lines_y, f.lines_x),
            InputKind::Ime(e) => self.on_ime(e),
            InputKind::KeyboardEnter { .. } => self.set_focus(true),
            InputKind::KeyboardLeave { .. } => self.set_focus(false),
            InputKind::Key {
                key,
                pressed,
                repeat,
                ..
            } => self.on_key(key, pressed, repeat),
            _ => {}
        }
    }

    fn set_focus(&mut self, focused: bool) {
        self.win_focused = focused;
        self.a11y_window_focus(focused);
        if !focused {
            // Nothing held survives the keyboard leaving: no table, no half-typed sequence.
            self.keys.reset();
            self.consumed.clear();
        }
        if let Some(id) = self.focused() {
            self.report_focus(id, focused);
        }
        self.poke();
    }

    /// Key to PTY: bind lookup, encode from the focused pane's lock-free mirror, enqueue.
    /// Never touches the term, and allocates nothing unless a bind fires.
    fn on_key(&mut self, key: u32, pressed: bool, repeat: bool) {
        let Some(focus) = self.focused() else { return };
        let Some(look) = self.kb.lookup(key) else {
            return;
        };
        if !pressed {
            if let Some(i) = self.consumed.iter().position(|c| c.key == key) {
                // The press was a bind's (or the interface's): the release is taken too.
                let c = self.consumed.swap_remove(i);
                if let Some(a) = c.on_release {
                    self.dispatch(a, 0);
                }
                return;
            }
            if self.palette.is_some()
                || self.paste.is_some()
                || self.nav.hints.is_some()
                || self.nav.consent.is_some()
                || self.nav.open.is_some()
                || self.auto.confirm.is_some()
                || self.nav.search.as_ref().is_some_and(|s| s.focused)
            {
                return;
            }
        }
        if self.nav.consent.is_some() || self.nav.open.is_some() || self.auto.confirm.is_some() {
            // A consent, link-confirm or rule bar owns the keyboard until answered (T-18 §3).
            if pressed && !repeat {
                self.consumed.push(Consumed::plain(key));
                self.answer_nav_bar(look.key);
            }
            return;
        }
        if self.paste.is_some() {
            // A paste confirm owns the keyboard until answered (T-08 §5: nothing typed reaches
            // the shell while the interface asks a question).
            if pressed && !repeat {
                self.consumed.push(Consumed::plain(key));
                self.answer_paste(look.key);
            }
            return;
        }
        if self.palette.is_some() {
            // The palette owns the keyboard while open (T-08 §5); repeats edit the query too.
            if pressed {
                if !repeat {
                    self.consumed.push(Consumed::plain(key));
                }
                self.palette_key(&look, key);
            }
            return;
        }
        if self.nav.hints.is_some() {
            // Hint mode owns the keyboard: typed keys pick a label (T-14 §7).
            if pressed {
                if !repeat {
                    self.consumed.push(Consumed::plain(key));
                }
                self.hint_key(&look);
            }
            return;
        }
        if self.nav.search.as_ref().is_some_and(|s| s.focused) {
            // The search bar owns the keyboard while focused (T-14 §6).
            if pressed {
                if !repeat {
                    self.consumed.push(Consumed::plain(key));
                }
                self.search_key(&look);
            }
            return;
        }
        if pressed
            && !repeat
            && let Some((backward, till)) = self.nav.await_jump.take()
        {
            // `cursor.jump-char` with no character: this key names it.
            self.consumed.push(Consumed::plain(key));
            if let Some(c) = look
                .text
                .as_str()
                .chars()
                .next()
                .filter(|c| !c.is_control())
            {
                self.jump(c, backward, till);
            }
            return;
        }
        if pressed
            && repeat
            && let Some(c) = self.consumed.iter().find(|c| c.key == key)
        {
            // A bind's key repeats the bind (when it asks to), never the program.
            if let Some(a) = c.on_repeat.clone() {
                self.dispatch(a, 0);
            }
            return;
        }
        if pressed && !repeat && look.key == Key::Named(Named::Escape) && self.try_dismiss_bar() {
            self.consumed.push(Consumed::plain(key));
            return;
        }
        let Some(p) = self.panes.get(&focus) else {
            return;
        };
        let modes = p.shared.mirror.load();
        if pressed
            && !repeat
            && let Some(bm) = binds::bind_mods(self.held)
        {
            let mut names = NameBuf::default();
            let ev = binds::bind_event(&look, key, bm, self.keymap.layout_independent, &mut names);
            let sel = self.mouse.has_selection();
            let copy = self.nav.copy == Some(focus);
            let searching = self.nav.search.is_some();
            let r = self.keys.press(&self.keymap, &ev, Instant::now(), &|m| {
                mode_holds(m, modes, sel, copy, searching)
            });
            match r {
                Resolved::None => {}
                Resolved::Prefix => {
                    self.consumed.push(Consumed::plain(key));
                    self.write_mux_dump();
                    return;
                }
                Resolved::Fire {
                    actions,
                    repeat: rep,
                    release,
                    performable: perf,
                } => {
                    if !perf || performable(&actions, modes) {
                        self.consumed.push(Consumed {
                            key,
                            on_repeat: (rep && !release).then(|| actions.clone()),
                            on_release: release.then(|| actions.clone()),
                        });
                        if !release {
                            self.dispatch(actions, 0);
                        }
                        return;
                    }
                }
            }
        }
        if self.nav.copy == Some(focus) {
            // Copy mode: keys its table does not bind go nowhere (never to the program).
            if pressed && !repeat {
                self.consumed.push(Consumed::plain(key));
            }
            return;
        }
        let kind = if !pressed {
            KeyKind::Release
        } else if repeat {
            KeyKind::Repeat
        } else {
            KeyKind::Press
        };
        let ev = look.event(kind);
        let mut buf = [0u8; 64];
        let n = encode_key(&ev, modes, &mut buf);
        if n == 0 {
            return;
        }
        if pressed {
            p.shared.last_key_ns.store(now_ns(), Ordering::Release);
            self.typed();
        }
        self.send_bytes(focus, &buf[..n]);
    }

    /// Typing (or pasting) drops the selection (`selection.clear-on-typing`) and snaps a
    /// scrolled view back to the bottom.
    fn typed(&mut self) {
        if self.settings.input.clear_on_typing && self.mouse.selection_gone() {
            self.view(ViewCmd::Clear);
        }
        if self.scrolled {
            self.view(ViewCmd::Scroll(ScrollBy::Bottom));
        }
    }

    /// A view command for the focused pane.
    fn view(&mut self, cmd: ViewCmd) {
        if let Some(id) = self.focused() {
            self.view_of(id, cmd);
        }
    }

    fn view_of(&mut self, id: PaneKey, cmd: ViewCmd) {
        if let ViewCmd::Scroll(by) = &cmd {
            self.scrolled = *by != ScrollBy::Bottom;
        }
        if let Some(tx) = &self.render_tx {
            let _ = tx.send(RenderMsg::View(id, cmd));
        }
    }

    fn send_view_settings(&mut self, id: PaneKey) {
        let i = &self.settings.input;
        let cmd = ViewCmd::Settings(ViewSettings {
            word_extra: i.word_extra.clone(),
            trim_trailing: i.trim_trailing,
        });
        self.view_of(id, cmd);
    }

    // ------------------------------------------------------------------ palette

    fn palette_tabs(&self) -> Vec<TabItem> {
        let Some(w) = self.session.window(self.mux_win) else {
            return Vec::new();
        };
        w.tabs
            .iter()
            .map(|&t| TabItem {
                title: self.tab_title(t),
                active: Some(t) == w.active,
            })
            .collect()
    }

    fn palette_rows(&self) -> usize {
        self.geo.map_or(1, |g| apoth_ui::palette::max_rows(g.area))
    }

    fn open_palette(&mut self, query: &str) {
        let mut p = Palette::new(pal::Mode::Actions, query);
        p.refresh(&self.keymap, &self.palette_tabs());
        self.palette = Some(p);
        self.ui_state = UiState::default();
        self.send_layout();
    }

    fn close_palette(&mut self) {
        if self.palette.take().is_some() {
            self.ui_state = UiState::default();
            self.send_layout();
        }
    }

    /// The query changed: filter again.
    fn palette_changed(&mut self) {
        let tabs = self.palette_tabs();
        if let Some(p) = self.palette.as_mut() {
            p.refresh(&self.keymap, &tabs);
        }
        self.send_layout();
    }

    /// A key while the palette is open. Query text is user input: drawn, never logged.
    fn palette_key(&mut self, look: &KeyLookup, code: u32) {
        let ctrl = self.held.contains(Mods::CTRL);
        let window = self.palette_rows();
        let Some(p) = self.palette.as_mut() else {
            return;
        };
        let changed = match look.key {
            Key::Named(Named::Escape) => return self.close_palette(),
            Key::Named(Named::Enter | Named::Kp(apoth_input::Kp::Enter)) => {
                return self.palette_pick();
            }
            Key::Named(Named::Up) => {
                p.move_by(-1, window);
                false
            }
            Key::Named(Named::Down) => {
                p.move_by(1, window);
                false
            }
            Key::Named(Named::PageUp) => {
                p.move_by(-(window as i32), window);
                false
            }
            Key::Named(Named::PageDown) => {
                p.move_by(window as i32, window);
                false
            }
            Key::Named(Named::Backspace) if ctrl => p.delete_word(),
            Key::Named(Named::Backspace) => p.backspace(),
            Key::Char('w') if ctrl => p.delete_word(),
            Key::Char('u') if ctrl => {
                p.set_query("");
                true
            }
            _ => {
                // The palette's own toggle and close binds dismiss it.
                if let Some(bm) = binds::bind_mods(self.held) {
                    let mut names = NameBuf::default();
                    let ev = binds::bind_event(
                        look,
                        code,
                        bm,
                        self.keymap.layout_independent,
                        &mut names,
                    );
                    let mut scratch = KeyState::default();
                    if let Resolved::Fire { actions, .. } =
                        scratch.press(&self.keymap, &ev, Instant::now(), &|_| false)
                        && actions.iter().any(|a| {
                            matches!(a, Action::PaletteToggle {} | Action::PaletteClose {})
                        })
                    {
                        return self.close_palette();
                    }
                }
                let plain = !self.held.contains(Mods::CTRL)
                    && !self.held.contains(Mods::ALT)
                    && !self.held.contains(Mods::SUPER);
                let Some(p) = self.palette.as_mut() else {
                    return;
                };
                plain && p.type_text(look.text.as_str())
            }
        };
        if changed {
            self.palette_changed();
        } else {
            self.send_layout();
        }
    }

    /// Run the selected row.
    fn palette_pick(&mut self) {
        let Some(pick) = self.palette.as_ref().and_then(|p| p.picked().cloned()) else {
            return;
        };
        match pick {
            Pick::Run(a) => {
                self.close_palette();
                self.dispatch(vec![a], 0);
            }
            Pick::Fill(q) => {
                if let Some(p) = self.palette.as_mut() {
                    p.set_query(&q);
                }
                self.palette_changed();
            }
            Pick::Tab(i) => {
                self.close_palette();
                self.focus_tab(TabTarget::Index(i));
            }
            Pick::Mode(m) => {
                if let Some(p) = self.palette.as_mut() {
                    p.mode = m;
                    p.set_query("");
                }
                self.palette_changed();
            }
        }
    }

    // ------------------------------------------------------------------ actions

    /// Run bound (or palette, or macro) actions in order (T-11 §2). Action ids are logged at
    /// debug level when not available yet; arguments never are (they can carry user text).
    fn dispatch(&mut self, actions: Vec<Action>, depth: u8) {
        for a in actions {
            let id = a.id();
            let ran = self.run_action(a, depth);
            debug_assert_eq!(ran, handles(id), "HANDLED is out of step with run_action");
        }
    }

    /// Run one action; `false` when it has no handler yet.
    fn run_action(&mut self, a: Action, depth: u8) -> bool {
        use Action as A;
        let focus = self.focused();
        match a {
            A::InputSendKeys { keys } => {
                if let Some(id) = focus {
                    self.typed();
                    let _ = self.send_chords(id, &keys.0);
                }
            }
            A::InputSendText { text } => self.send_to_focus(text.into_bytes()),
            A::InputSendBytes { bytes } => self.send_to_focus(bytes.0),
            A::InputSendCsi { body } => {
                let mut b = b"\x1b[".to_vec();
                b.extend_from_slice(body.as_bytes());
                self.send_to_focus(b);
            }
            A::InputSendEsc { body } => {
                let mut b = b"\x1b".to_vec();
                b.extend_from_slice(body.as_bytes());
                self.send_to_focus(b);
            }
            A::InputPaste { text: Some(t) } => self.do_paste(t.into_bytes(), Origin::Selection),
            A::InputPaste { text: None } => self.request_paste(SelectionKind::Clipboard),
            A::InputPasteSelection {} => {
                if self.settings.input.primary {
                    self.request_paste(SelectionKind::Primary);
                }
            }
            A::ClipboardCopy { text } => self.on_copied(Copied {
                target: CopyTarget::Clipboard,
                text: clip(&text, apoth_action::arg::MAX_TEXT_BYTES).to_owned(),
            }),
            A::SelectionCopy { board } => {
                let target = match board {
                    Some(apoth_action::enums::Board::Primary) => CopyTarget::Primary,
                    _ => CopyTarget::Clipboard,
                };
                self.view(ViewCmd::Copy {
                    target,
                    clear: self.settings.input.clear_on_copy,
                });
            }
            A::SelectionSelectAll {} => {
                self.mouse.mark_selection();
                self.view(ViewCmd::All);
            }
            A::SelectionClear {} => {
                self.mouse.selection_gone();
                self.view(ViewCmd::Clear);
            }
            A::ScrollPageUp {} => self.scroll(ScrollBy::Pages(1)),
            A::ScrollPageDown {} => self.scroll(ScrollBy::Pages(-1)),
            A::ScrollHalfPageUp {} => self.scroll(ScrollBy::HalfPages(1)),
            A::ScrollHalfPageDown {} => self.scroll(ScrollBy::HalfPages(-1)),
            A::ScrollLineUp { lines } => {
                self.scroll(ScrollBy::Lines(lines.unwrap_or(1).min(10_000) as i32));
            }
            A::ScrollLineDown { lines } => {
                self.scroll(ScrollBy::Lines(-(lines.unwrap_or(1).min(10_000) as i32)));
            }
            A::ScrollTop {} => self.scroll(ScrollBy::Top),
            A::ScrollBottom {} => self.scroll(ScrollBy::Bottom),
            A::KeyTablePush {
                name,
                one_shot,
                timeout,
            } => match self.keymap.table_index(&name) {
                Some(i) => {
                    let t = timeout.map(|d| Duration::from_millis(d.0));
                    self.keys.push(i, one_shot, t, Instant::now());
                    self.write_mux_dump();
                }
                None => tracing::debug!("key-table.push names no table"),
            },
            A::KeyTablePop {} => {
                self.keys.pop();
                self.write_mux_dump();
            }
            A::KeyTableClear {} => {
                self.keys.clear();
                self.write_mux_dump();
            }
            A::PaletteToggle {} => {
                if self.palette.is_some() {
                    self.close_palette();
                } else {
                    self.open_palette("");
                }
            }
            A::PaletteOpen { query } => self.open_palette(query.as_deref().unwrap_or("")),
            A::PaletteClose {} => self.close_palette(),
            A::PaneSplit {
                direction,
                argv,
                cwd,
                profile,
            } => {
                let argv = argv.and_then(|a| self.expand_argv(&a));
                let Some((argv, cwd, env)) =
                    self.with_profile(profile.as_deref(), argv, cwd.map(PathBuf::from))
                else {
                    return true;
                };
                if let Some(id) = self.split(split_dir(direction), argv, cwd, env)
                    && let Some(p) = profile
                {
                    self.set_explicit_profile(id, &p);
                }
            }
            A::PaneClose { force: _ } => {
                // HOOK(close.confirm): the running-process confirmation sheet is not built
                // yet; every close is immediate (reported gap).
                if let Some(id) = focus {
                    self.close_pane(id);
                }
            }
            A::PaneFocus { direction } => self.with_tab(|t, _| {
                let _ = t.focus_move(FocusMove::Dir(mux_dir(direction)));
            }),
            A::PaneFocusNext {} => self.with_tab(|t, _| {
                let _ = t.focus_move(FocusMove::Next);
            }),
            A::PaneFocusPrev {} => self.with_tab(|t, _| {
                let _ = t.focus_move(FocusMove::Prev);
            }),
            A::PaneFocusLast {} => self.with_tab(|t, _| {
                let _ = t.focus_move(FocusMove::Last);
            }),
            A::PaneMove { direction } => self.with_tab(|t, f| {
                let _ = t.swap_dir(f, mux_dir(direction));
            }),
            A::PaneResize { direction, amount } => self.with_tab(|t, f| {
                let n = amount.unwrap_or(1).min(1000) as i32;
                let _ = t.resize(f, mux_dir(direction), n);
            }),
            A::PaneToggleZoom {} => self.with_tab(|t, _| {
                let _ = t.zoom_toggle();
            }),
            A::PaneEqualize {} => self.with_tab(|t, _| t.equalize()),
            A::PaneBreakOut {} => {
                if let Some(id) = focus
                    && self.session.break_to_tab(id).is_ok()
                {
                    self.relayout();
                }
            }
            A::LayoutNext {} => self.with_tab(|t, _| {
                let _ = t.next_preset();
            }),
            A::TabNew { argv, cwd, profile } => {
                let argv = argv.and_then(|a| self.expand_argv(&a));
                let Some((argv, cwd, env)) =
                    self.with_profile(profile.as_deref(), argv, cwd.map(PathBuf::from))
                else {
                    return true;
                };
                if let Some(id) = self.new_tab_with(argv, cwd, env)
                    && let Some(p) = profile
                {
                    self.set_explicit_profile(id, &p);
                }
            }
            A::TabClose {} => {
                if let Some(t) = self.active_tab() {
                    self.close_tab(t);
                }
            }
            A::TabCloseOthers {} => {
                let keep = self.active_tab();
                let others: Vec<TabId> = self
                    .session
                    .window(self.mux_win)
                    .map(|w| {
                        w.tabs
                            .iter()
                            .copied()
                            .filter(|t| Some(*t) != keep)
                            .collect()
                    })
                    .unwrap_or_default();
                for t in others {
                    self.close_tab(t);
                }
            }
            A::TabNext {} => self.focus_tab(TabTarget::Next),
            A::TabPrev {} => self.focus_tab(TabTarget::Prev),
            A::TabLast {} => self.focus_tab(TabTarget::Last),
            A::TabSelect { index } => {
                if index > 0 {
                    self.focus_tab(TabTarget::Index(index as usize - 1));
                }
            }
            A::TabMove { delta } => {
                if let (Some(t), Some(w)) = (self.active_tab(), self.session.window(self.mux_win)) {
                    let cur = w.tabs.iter().position(|x| *x == t).unwrap_or(0) as i64;
                    let to = (cur + i64::from(delta)).clamp(0, w.tabs.len() as i64 - 1) as usize;
                    if self.session.move_tab(t, to).is_ok() {
                        self.relayout();
                    }
                }
            }
            A::TabRename { title } => {
                if let Some(t) = self.active_tab() {
                    let name = title.as_deref().map_or("", |s| clip(s, MAX_TITLE));
                    let _ = self.session.rename_tab(t, name);
                    self.send_layout();
                }
            }
            A::MacroRun { name } => {
                // Macros are checked for recursion at build (T-11 §4); the depth cap is a
                // second fence.
                if depth < MAX_MACRO_DEPTH
                    && let Some(m) = self.keymap.macros.get(&name)
                {
                    let members = m.members.clone();
                    self.dispatch(members, depth + 1);
                }
            }
            A::SearchStart { query } => self.open_search(query),
            A::SearchSet { query } => self.open_search(Some(query)),
            A::SearchNext {} => self.search_step(true),
            A::SearchPrevious {} => self.search_step(false),
            A::SearchCancel {} => self.close_search(),
            A::SearchToggleCase {} => {
                if let Some(s) = self.nav.search.as_mut() {
                    s.query.case = match s.query.case {
                        Case::Smart => Case::Sensitive,
                        Case::Sensitive => Case::Insensitive,
                        Case::Insensitive => Case::Smart,
                    };
                    self.search_now();
                }
            }
            A::SearchToggleRegex {} => {
                if let Some(s) = self.nav.search.as_mut() {
                    s.query.regex = !s.query.regex;
                    self.search_now();
                }
            }
            A::CopyModeEnter {} => self.enter_copy(),
            A::CopyModeExit {} => self.exit_copy(),
            A::CopyModeToggle {} => {
                if self.nav.copy.is_some() {
                    self.exit_copy();
                } else {
                    self.enter_copy();
                }
            }
            A::CopyModeMove { motion, count } => {
                if let Some(id) = self.nav.copy {
                    self.view_of(
                        id,
                        ViewCmd::CopyMove {
                            motion,
                            count: count.unwrap_or(1),
                        },
                    );
                }
            }
            A::CopyModeToggleSelection { unit } => {
                use apoth_action::enums::SelectUnit;
                if let Some(id) = self.nav.copy {
                    let kind = match unit {
                        Some(SelectUnit::Word) => apoth_grid::SelKind::Word,
                        Some(SelectUnit::Line) => apoth_grid::SelKind::Line,
                        Some(SelectUnit::Block) => apoth_grid::SelKind::Block,
                        Some(SelectUnit::Cell) | None => apoth_grid::SelKind::Simple,
                    };
                    self.mouse.mark_selection();
                    self.view_of(id, ViewCmd::CopySelect(kind));
                }
            }
            A::CursorJumpChar { ch, backward, till } => {
                if self.nav.copy.is_some() {
                    match ch {
                        Some(c) => self.jump(c, backward, till),
                        None => self.nav.await_jump = Some((backward, till)),
                    }
                }
            }
            A::CursorJumpRepeat { reverse } => {
                if let Some((c, backward, till)) = self.nav.last_jump
                    && let Some(id) = self.nav.copy
                {
                    self.view_of(
                        id,
                        ViewCmd::CopyJump {
                            ch: c,
                            backward: backward ^ reverse,
                            till,
                        },
                    );
                }
            }
            A::HintsStart { hint } => self.start_hints(hint.as_deref()),
            A::HintsCancel {} => {
                if let Some(id) = self.nav.hints {
                    self.view_of(id, ViewCmd::HintsCancel);
                }
            }
            A::BlockJump { direction } => {
                self.scrolled = true;
                self.view(ViewCmd::BlockJump {
                    older: direction == apoth_action::enums::Order::Prev,
                });
            }
            A::Open { target, argv } => {
                let argv = match argv {
                    Some(a) => match self.expand_argv(&a) {
                        Some(v) => Some(v),
                        None => return true,
                    },
                    None => None,
                };
                // A URI is held to the link rules; a bare path opens as given.
                if (target.contains("://") || target.starts_with("mailto:"))
                    && let Err(e) =
                        security::check_link(&target, &self.settings.nav.schemes, &self.hostname)
                {
                    tracing::info!(reason = e.reason(), "open refused");
                    return true;
                }
                self.spawn_open(&target, argv);
            }
            A::ConfigReload {} => self.start_reload(),
            A::Notify {
                title,
                body,
                urgency,
            } => self.notify_action(&title, body.as_deref(), urgency),
            A::VarSet { name, value, scope } => self.var_action(&name, Some(&value), scope),
            A::VarUnset { name, scope } => self.var_action(&name, None, scope),
            A::AppQuit { force: _ } | A::WindowClose {} => self.begin_close(),
            A::AppIgnore {} => {}
            other => {
                tracing::debug!(action = other.id(), "action not available yet");
                return false;
            }
        }
        true
    }

    /// Encode key chords for pane `id` through the key encoder under its current modes
    /// (T-12 §2: `input.send-keys` never enters bind lookup). An unknown key name fails the
    /// whole call before anything is sent.
    fn send_chords(
        &mut self,
        id: PaneKey,
        chords: &[apoth_action::KeyChord],
    ) -> Result<(), &'static str> {
        let Some(p) = self.panes.get(&id) else {
            return Err("no such pane");
        };
        let modes = p.shared.mirror.load();
        let mut bytes = Vec::new();
        for c in chords {
            let apoth_action::key::Key::Symbol(name) = &c.key else {
                return Err("physical keys cannot be sent");
            };
            let Some((key, text, shift)) = apoth_input::backend_kbvm::key_of_name(name) else {
                return Err("unknown key name");
            };
            let mut mods = Mods::NONE;
            for (a, b) in [
                (apoth_action::Mods::SHIFT, Mods::SHIFT),
                (apoth_action::Mods::CTRL, Mods::CTRL),
                (apoth_action::Mods::ALT, Mods::ALT),
                (apoth_action::Mods::SUPER, Mods::SUPER),
            ] {
                if c.mods.contains(a) {
                    mods = mods | b;
                }
            }
            if shift {
                mods = mods | Mods::SHIFT;
            }
            let mut tbuf = [0u8; 4];
            let t = text.map_or("", |ch| ch.encode_utf8(&mut tbuf));
            let ev = apoth_input::KeyEvent::new(key).mods(mods).text(t);
            let mut out = [0u8; 64];
            let n = encode_key(&ev, modes, &mut out);
            bytes.extend_from_slice(&out[..n]);
        }
        if !bytes.is_empty() {
            self.send_unit(id, bytes);
        }
        Ok(())
    }

    /// Placeholders a pane command may name, from the focused pane (T-11 §5). Expansion
    /// failure (a placeholder with no value) runs nothing.
    fn expand_argv(&self, argv: &apoth_action::Argv) -> Option<Vec<String>> {
        let vals = PlaceholderValues {
            cwd: self
                .inherited_cwd()
                .map(|p| p.to_string_lossy().into_owned()),
            pane_id: self.focused().map(|p| p.to_string()),
            ..PlaceholderValues::default()
        };
        argv.expand(&vals).ok()
    }

    fn scroll(&mut self, by: ScrollBy) {
        let alt = self
            .focused()
            .and_then(|id| self.panes.get(&id))
            .is_some_and(|p| p.shared.mirror.load().has(bit::ALT_SCREEN));
        if !alt {
            self.view(ViewCmd::Scroll(by));
        }
    }

    fn send_to_focus(&mut self, bytes: Vec<u8>) {
        if let Some(id) = self.focused()
            && !bytes.is_empty()
        {
            self.typed();
            self.send_unit(id, bytes);
        }
    }

    // -------------------------------------------------- selection, clipboard, paste

    /// Text the view took from the selection goes on the seat as ours.
    fn on_copied(&mut self, c: Copied) {
        let Some(seat) = self.seat else { return };
        let kind = match c.target {
            CopyTarget::Clipboard => SelectionKind::Clipboard,
            CopyTarget::Primary => SelectionKind::Primary,
        };
        let n = c.text.len();
        if kind == SelectionKind::Clipboard {
            // The clipboard now holds what the user copied, not program content.
            self.nav.osc52_tag = None;
        }
        match self.handle.set_selection(seat, kind, c.text.into_bytes()) {
            Ok(()) => tracing::debug!(bytes = n, ?kind, "selection offered"),
            Err(e) => tracing::warn!(error = %e, "cannot offer the selection"),
        }
    }

    /// Start reading a selection to paste it; the transfer runs on the loop.
    fn request_paste(&mut self, kind: SelectionKind) {
        let Some(seat) = self.seat else { return };
        match self.handle.receive_selection(seat, kind, Want::Text) {
            Ok(t) => {
                if t.insert(&self.lh, |d: TransferDone, app: &mut App| {
                    app.on_transfer(d)
                })
                .is_err()
                {
                    tracing::warn!("cannot start a paste transfer");
                }
            }
            Err(e) => tracing::debug!(error = %e, "nothing to paste"),
        }
    }

    /// A drop was released over the window: read it and send it through the paste path.
    fn start_drop(&mut self, seat: SeatId) {
        match self.handle.receive_drop(seat) {
            Ok(t) => {
                self.drop_ids.push(t.id());
                if t.insert(&self.lh, |d: TransferDone, app: &mut App| {
                    app.on_transfer(d)
                })
                .is_err()
                {
                    self.handle.finish_drop(seat, false);
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "drop refused");
                self.handle.finish_drop(seat, false);
            }
        }
    }

    fn on_transfer(&mut self, d: TransferDone) {
        let drop = self
            .drop_ids
            .iter()
            .position(|i| *i == d.id)
            .map(|i| self.drop_ids.swap_remove(i))
            .is_some();
        if d.direction != TransferDirection::Receive {
            return;
        }
        if let Some(i) = self.nav.osc52_reads.iter().position(|r| r.0 == d.id) {
            // A permitted OSC 52 read: answer with the content as one reply unit.
            let (_, pane, sel) = self.nav.osc52_reads.swap_remove(i);
            let data = if d.status == TransferStatus::Complete {
                d.data.0
            } else {
                Vec::new()
            };
            let unit = nav::osc52_reply(&sel, &data, self.settings.term.osc52_max);
            tracing::debug!(bytes = data.len(), "osc 52 read answered");
            self.send_unit(pane, unit);
            return;
        }
        let ok = d.status == TransferStatus::Complete;
        if drop {
            self.handle.finish_drop(d.seat, ok);
        }
        if !ok {
            tracing::debug!(status = ?d.status, "paste transfer failed");
            return;
        }
        let data = d.data.0;
        if drop {
            let content = DropContent::from_bytes(d.payload, &data);
            if content.is_empty() {
                return;
            }
            let origin = match content {
                DropContent::Paths(_) => Origin::DropFiles,
                DropContent::Text(_) => Origin::DropText,
            };
            self.do_paste(content.paste_bytes(), origin);
        } else {
            self.do_paste(data, Origin::Selection);
        }
    }

    /// The paste path (T-18 §4): the guard decides, then one encoded unit goes to the focused
    /// pane's PTY.
    fn do_paste(&mut self, bytes: Vec<u8>, origin: Origin) {
        let Some(id) = self.focused() else { return };
        let Some(p) = self.panes.get(&id) else { return };
        let modes = p.shared.mirror.load();
        if bytes.is_empty() || self.paste.is_some() {
            return;
        }
        let target = paste::Target {
            at_prompt: p.shared.at_prompt.load(Ordering::Acquire),
            bracketed_ok: p.shared.bracketed_ok.load(Ordering::Acquire),
            osc52: origin == Origin::Selection
                && self
                    .nav
                    .osc52_tag
                    .is_some_and(|(h, _)| h == nav::content_hash(&bytes)),
        };
        match paste::needs_confirm_at(&bytes, origin, modes, &self.settings.input.paste, target) {
            None => self.send_paste(id, &bytes),
            Some(reason) => {
                tracing::debug!(?reason, bytes = bytes.len(), "paste needs confirmation");
                let stats = paste::stats(&bytes);
                self.paste = Some(PendingPaste {
                    bytes,
                    stats,
                    pane: id,
                });
                self.bar_dismissed = false;
                self.refresh_bar();
            }
        }
    }

    // ------------------------------------------------------------ navigation

    /// End search, hints and copy mode of pane `id` (focus left it or it closed).
    fn end_nav(&mut self, id: PaneKey) {
        if self.nav.copy == Some(id) {
            self.exit_copy();
        }
        if self.nav.hints == Some(id) {
            self.nav.hints = None;
            self.view_of(id, ViewCmd::HintsCancel);
        }
        if self.nav.search.as_ref().is_some_and(|s| s.pane == id) {
            self.close_search();
        }
        if self.nav.link_tip.take().is_some() {
            self.send_layout();
        }
        self.nav.hover_cell = None;
    }

    fn open_search(&mut self, query: Option<String>) {
        let Some(pane) = self.focused() else { return };
        let fire = query.is_some();
        match self.nav.search.as_mut() {
            Some(s) if s.pane == pane => {
                s.focused = true;
                if let Some(q) = query {
                    s.query.text = q;
                }
            }
            _ => {
                if let Some(old) = self.nav.search.take() {
                    self.view_of(old.pane, ViewCmd::Search(None));
                }
                self.nav.search = Some(SearchUi {
                    pane,
                    query: Query {
                        text: query.unwrap_or_default(),
                        ..Query::default()
                    },
                    status: crate::search::Status::default(),
                    focused: true,
                    generation: 0,
                });
            }
        }
        if fire {
            self.search_now();
        } else {
            self.send_layout();
        }
    }

    fn close_search(&mut self) {
        if let Some(s) = self.nav.search.take() {
            self.view_of(s.pane, ViewCmd::Search(None));
            self.send_layout();
        }
    }

    fn search_step(&mut self, older: bool) {
        if let Some(pane) = self.nav.search.as_ref().map(|s| s.pane) {
            self.scrolled = true;
            self.view_of(pane, ViewCmd::SearchStep { older });
        }
    }

    /// Restart the search with the current query at once (mode toggles, `search.set`).
    fn search_now(&mut self) {
        let Some(s) = self.nav.search.as_mut() else {
            return;
        };
        s.generation += 1;
        let generation = s.generation;
        self.send_layout();
        self.search_fire(generation);
    }

    /// The query was typed: restart the search after the debounce (T-14 §6: 150 ms).
    fn search_changed(&mut self) {
        let Some(s) = self.nav.search.as_mut() else {
            return;
        };
        s.generation += 1;
        let generation = s.generation;
        self.send_layout();
        let _ = self.lh.insert_source(
            Timer::from_duration(Duration::from_millis(nav::DEBOUNCE_MS)),
            move |_, (), app: &mut App| {
                app.search_fire(generation);
                TimeoutAction::Drop
            },
        );
    }

    fn search_fire(&mut self, generation: u64) {
        let Some(s) = self.nav.search.as_ref() else {
            return;
        };
        if s.generation != generation {
            return;
        }
        let (pane, q) = (s.pane, s.query.clone());
        self.scrolled = true;
        self.view_of(pane, ViewCmd::Search(Some(q)));
    }

    /// A key while the search bar has the keyboard.
    fn search_key(&mut self, look: &KeyLookup) {
        enum Op {
            Close,
            Unfocus,
            Step(bool),
            Edited,
            Chip(Chip),
            Nothing,
        }
        let shift = self.held.contains(Mods::SHIFT);
        let alt = self.held.contains(Mods::ALT);
        let ctrl = self.held.contains(Mods::CTRL) || self.held.contains(Mods::SUPER);
        let copy = self.nav.copy;
        let Some(s) = self.nav.search.as_mut() else {
            return;
        };
        let in_copy = copy == Some(s.pane);
        let op = match look.key {
            Key::Named(Named::Escape) if in_copy => Op::Unfocus,
            Key::Named(Named::Escape) => Op::Close,
            Key::Named(Named::Enter | Named::Kp(apoth_input::Kp::Enter)) => {
                if in_copy {
                    s.focused = false;
                }
                Op::Step(!shift)
            }
            Key::Named(Named::Up) => Op::Step(true),
            Key::Named(Named::Down) => Op::Step(false),
            Key::Char('c') if alt => Op::Chip(Chip::Case),
            Key::Char('r') if alt => Op::Chip(Chip::Regex),
            Key::Char('w') if alt => Op::Chip(Chip::Word),
            Key::Char('s') if alt => Op::Chip(Chip::Scope),
            Key::Char('u') if ctrl => {
                s.query.text.clear();
                Op::Edited
            }
            _ if alt || ctrl => Op::Nothing,
            k => {
                if nav::edit_query(&mut s.query.text, k, Some(look.text.as_str())) {
                    Op::Edited
                } else {
                    Op::Nothing
                }
            }
        };
        match op {
            Op::Close => self.close_search(),
            Op::Unfocus => {
                if let Some(s) = self.nav.search.as_mut() {
                    s.focused = false;
                }
                self.send_layout();
            }
            Op::Step(older) => {
                self.search_step(older);
                self.send_layout();
            }
            Op::Edited => self.search_changed(),
            Op::Chip(c) => self.search_chip(c),
            Op::Nothing => {}
        }
    }

    fn enter_copy(&mut self) {
        let Some(pane) = self.focused() else { return };
        if self.nav.copy.is_some() {
            return;
        }
        self.nav.copy = Some(pane);
        if let Some(i) = self.keymap.table_index("copy-mode") {
            self.keys.push(i, false, None, Instant::now());
        }
        self.scrolled = true;
        self.view_of(pane, ViewCmd::CopyEnter);
        self.write_mux_dump();
    }

    fn exit_copy(&mut self) {
        let Some(pane) = self.nav.copy.take() else {
            return;
        };
        self.nav.await_jump = None;
        for name in ["copy-mode", "copy-mode-emacs"] {
            if let Some(i) = self.keymap.table_index(name) {
                self.keys.remove(i);
            }
        }
        self.view_of(pane, ViewCmd::CopyExit);
        if self
            .nav
            .search
            .as_ref()
            .is_some_and(|s| s.pane == pane && !s.focused)
        {
            self.close_search();
        }
        self.view_of(pane, ViewCmd::Scroll(ScrollBy::Bottom));
        self.write_mux_dump();
    }

    fn jump(&mut self, ch: char, backward: bool, till: bool) {
        self.nav.last_jump = Some((ch, backward, till));
        if let Some(id) = self.nav.copy {
            self.view_of(id, ViewCmd::CopyJump { ch, backward, till });
        }
    }

    /// `hints.start [set]`: `set` names built-in matchers, comma separated.
    fn start_hints(&mut self, set: Option<&str>) {
        let Some(pane) = self.focused() else { return };
        let kinds: Vec<crate::hints::Kind> = match set {
            Some(s) => {
                let k: Vec<_> = s
                    .split(',')
                    .filter_map(|n| crate::hints::Kind::parse(n.trim()))
                    .collect();
                if k.is_empty() {
                    // HOOK(T-14 §7): user regex matcher sets from KDL are not loaded yet.
                    tracing::debug!("hints.start names no built-in matcher");
                    return;
                }
                k
            }
            None => Vec::new(),
        };
        self.nav.hints = Some(pane);
        self.view_of(
            pane,
            ViewCmd::Hints {
                kinds,
                alphabet: self.settings.nav.hint_alphabet.clone(),
                max: self.settings.nav.hint_max,
            },
        );
    }

    fn hint_key(&mut self, look: &KeyLookup) {
        let Some(pane) = self.nav.hints else { return };
        let cmd = match look.key {
            Key::Named(Named::Escape) => ViewCmd::HintsCancel,
            Key::Named(Named::Backspace) => ViewCmd::HintKey('\u{8}'),
            _ => match look
                .text
                .as_str()
                .chars()
                .next()
                .filter(|c| !c.is_control())
            {
                Some(c) => ViewCmd::HintKey(c),
                None => return,
            },
        };
        self.view_of(pane, cmd);
    }

    fn on_nav(&mut self, id: PaneKey, m: NavMsg) {
        match m {
            NavMsg::Search(st) => {
                if let Some(s) = self.nav.search.as_mut()
                    && s.pane == id
                {
                    s.status = st;
                    self.send_layout();
                }
            }
            NavMsg::Hint { kind, text } => {
                if kind.opens() {
                    // A link hint's visible text is not the URI: always show it first.
                    let visible = if kind == crate::hints::Kind::Link {
                        ""
                    } else {
                        text.as_str()
                    };
                    self.open_link(&text, visible);
                } else {
                    self.on_copied(Copied {
                        target: CopyTarget::Clipboard,
                        text,
                    });
                }
            }
            NavMsg::HintsEnded => {
                if self.nav.hints == Some(id) {
                    self.nav.hints = None;
                }
            }
            NavMsg::Link(uri) => {
                if self.focused() == Some(id) && self.nav.link_tip != uri {
                    self.nav.link_tip = uri;
                    self.send_layout();
                }
            }
            NavMsg::Open { uri, visible } => self.open_link(&uri, &visible),
            NavMsg::CopyModeEnded => {
                if self.nav.copy == Some(id) {
                    self.exit_copy();
                }
            }
        }
    }

    /// T-14 §5: allow-listed scheme, no remote `file://`, and the full URI confirmed first
    /// when it differs from the visible text.
    fn open_link(&mut self, uri: &str, visible: &str) {
        match security::check_link(uri, &self.settings.nav.schemes, &self.hostname) {
            Err(e) => {
                tracing::info!(reason = e.reason(), "link not opened");
                self.handle.ring_bell(Some(self.win));
            }
            Ok(()) if security::needs_reveal(uri, visible) => {
                self.nav.open = Some(PendingOpen {
                    uri: uri.to_string(),
                });
                self.bar_dismissed = false;
                self.refresh_bar();
            }
            Ok(()) => self.spawn_open(uri, None),
        }
    }

    /// Run the opener argv (`links.open-command`, `{url}` substituted as one argument) on a
    /// worker; a reaper thread waits for it so the pool is never held by a long-lived opener.
    fn spawn_open(&mut self, target: &str, argv: Option<Vec<String>>) {
        let template = argv.unwrap_or_else(|| self.settings.nav.open_command.clone());
        let Some(argv) = security::open_argv(&template, target) else {
            tracing::info!("open: no opener command");
            return;
        };
        let job = move || {
            let mut c = std::process::Command::new(&argv[0]);
            c.args(&argv[1..])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .env_remove("XDG_ACTIVATION_TOKEN")
                .env_remove("DESKTOP_STARTUP_ID");
            match c.spawn() {
                Ok(mut child) => {
                    let _ = std::thread::Builder::new()
                        .name("apoth-open-reap".into())
                        .spawn(move || {
                            let _ = child.wait();
                        });
                }
                Err(e) => tracing::warn!(error = %e.kind(), "opener did not start"),
            }
        };
        if self.workers.submit(Box::new(job)).is_err() {
            tracing::warn!("worker queue full: open dropped");
        }
    }

    fn open_modifier_held(&self) -> bool {
        let want = self
            .settings
            .nav
            .open_modifier
            .split('+')
            .fold(Mods::NONE, |m, k| {
                m | match k.trim() {
                    "shift" => Mods::SHIFT,
                    "alt" => Mods::ALT,
                    "super" => Mods::SUPER,
                    _ => Mods::CTRL,
                }
            });
        self.held.contains(want)
    }

    /// Remote test (T-18 §10, process detection): a pane without a live PTY counts as
    /// remote (tighten only).
    fn pane_remote(&self, id: PaneKey) -> bool {
        self.panes
            .get(&id)
            .and_then(|p| p.pty.as_ref())
            .is_none_or(|pty| pty.foreground_is_remote(&[]))
    }

    /// OSC 52 writes the term let through: the policy decides per pane (remote re-checked
    /// here), and allowed content is offered on the seat and tagged for paste rule (e).
    fn on_osc52_write(&mut self, id: PaneKey, sets: Vec<apoth_term::ClipboardSet>) {
        let remote = self.pane_remote(id);
        let Some(seat) = self.seat else { return };
        for c in sets {
            let primary = c.selection.contains(['p', 's']);
            let d = if primary {
                self.settings.nav.policy.decide_primary(false, remote)
            } else {
                self.settings
                    .nav
                    .policy
                    .decide(Class::ClipboardWrite, remote)
            };
            if d != Decision::Allow || (primary && !self.settings.input.primary) {
                tracing::debug!(
                    class = Class::ClipboardWrite.id(),
                    remote,
                    "osc 52 write denied"
                );
                continue;
            }
            let kind = if primary {
                SelectionKind::Primary
            } else {
                SelectionKind::Clipboard
            };
            let n = c.data.len();
            let tag = nav::content_hash(&c.data);
            match self.handle.set_selection(seat, kind, c.data) {
                Ok(()) => {
                    if !primary {
                        self.nav.osc52_tag = Some((tag, remote));
                    }
                    tracing::debug!(bytes = n, "osc 52 write offered");
                }
                Err(e) => tracing::debug!(error = %e, "osc 52 write not offered"),
            }
        }
    }

    /// An OSC 52 read request: allow answers, ask shows the consent bar (rate limited),
    /// deny does nothing at all.
    fn on_osc52_read(&mut self, id: PaneKey, sel: String) {
        let remote = self.pane_remote(id);
        let primary = sel.contains(['p', 's']);
        let d = if primary {
            self.settings.nav.policy.decide_primary(true, remote)
        } else {
            self.settings
                .nav
                .policy
                .decide(Class::ClipboardRead, remote)
        };
        match d {
            Decision::Deny => tracing::debug!(class = Class::ClipboardRead.id(), remote, "denied"),
            Decision::Allow => self.osc52_read(id, sel),
            Decision::Ask => match self
                .nav
                .consents
                .ask(id, Class::ClipboardRead, Instant::now())
            {
                security::Ask::Granted => self.osc52_read(id, sel),
                security::Ask::AutoDeny | security::Ask::Busy => {
                    tracing::debug!(class = Class::ClipboardRead.id(), "consent rate-limited");
                }
                security::Ask::Show => {
                    if self.bar_busy() {
                        // One question on screen at a time: this one is dropped.
                        self.nav.consents.cancel(id, Class::ClipboardRead);
                        return;
                    }
                    self.nav.consent = Some(PendingConsent {
                        pane: id,
                        class: Class::ClipboardRead,
                        selection: sel,
                        token: 0,
                    });
                    self.bar_dismissed = false;
                    self.refresh_bar();
                }
            },
        }
    }

    /// The pane deny word: the policy's local column plus `graphics.file-transmission = off`.
    fn deny_word(&self) -> u32 {
        let mut w = self.settings.nav.policy.deny_word(false);
        if !self.settings.graphics_file {
            w |= apoth_term::deny::GRAPHICS_FILE;
            w &= !apoth_term::deny::GRAPHICS_FILE_ASK;
        }
        w
    }

    /// A kitty transmission asks to read a file (T-15 §5, T-18 §3 `graphics-file`): the
    /// pane's output waits for the answer, so every path answers (a dropped question is a
    /// deny). Nothing about the file is logged.
    fn on_graphics_file(&mut self, id: PaneKey, token: u64) {
        let remote = self.pane_remote(id);
        let allow = match self.settings.nav.policy.decide(Class::GraphicsFile, remote) {
            Decision::Allow => true,
            Decision::Deny => false,
            Decision::Ask => match self
                .nav
                .consents
                .ask(id, Class::GraphicsFile, Instant::now())
            {
                security::Ask::Granted => true,
                security::Ask::AutoDeny | security::Ask::Busy => false,
                security::Ask::Show => {
                    if self.bar_busy() {
                        self.nav.consents.cancel(id, Class::GraphicsFile);
                        false
                    } else {
                        self.nav.consent = Some(PendingConsent {
                            pane: id,
                            class: Class::GraphicsFile,
                            selection: String::new(),
                            token,
                        });
                        self.bar_dismissed = false;
                        self.refresh_bar();
                        return;
                    }
                }
            },
        };
        tracing::debug!(class = Class::GraphicsFile.id(), remote, allow, "decided");
        self.graphics_file_answer(id, token, allow);
    }

    fn graphics_file_answer(&self, id: PaneKey, token: u64, allow: bool) {
        let Some(p) = self.panes.get(&id) else { return };
        p.shared.post_graphics_file(token, allow);
        if let Some(io) = &p.io {
            let _ = rustix::io::write(&io.wake, &[WAKE_GRAPHICS_FILE]);
        }
    }

    /// Read the seat selection to answer an OSC 52 query; nothing to read answers empty.
    fn osc52_read(&mut self, id: PaneKey, sel: String) {
        let kind = if sel.contains(['p', 's']) {
            SelectionKind::Primary
        } else {
            SelectionKind::Clipboard
        };
        let started = self.seat.and_then(|seat| {
            let t = self.handle.receive_selection(seat, kind, Want::Text).ok()?;
            let tid = t.id();
            t.insert(&self.lh, |d: TransferDone, app: &mut App| {
                app.on_transfer(d)
            })
            .ok()?;
            Some(tid)
        });
        match started {
            Some(tid) => self.nav.osc52_reads.push((tid, id, sel)),
            None => {
                let unit = nav::osc52_reply(&sel, b"", self.settings.term.osc52_max);
                self.send_unit(id, unit);
            }
        }
    }

    /// A key answers the consent or link-confirm bar (explicit keys only, after the guard).
    fn answer_nav_bar(&mut self, key: apoth_input::Key) {
        let Some(o) = &self.bar else { return };
        let age = o.shown_at.elapsed().as_millis() as u64;
        if self.auto.confirm.is_some() && self.nav.consent.is_none() && self.nav.open.is_none() {
            // The rule sheet has no Escape button: Escape (after the guard) dismisses it.
            let esc = matches!(key, apoth_input::Key::Named(apoth_input::Named::Escape));
            match paste_answer(&o.spec, age, key) {
                Some(a) => self.answer_rule(Some(a)),
                None if esc && age >= apoth_ui::tokens::motion::CONSENT_GUARD_MS => {
                    self.answer_rule(None);
                }
                None => {}
            }
            return;
        }
        let Some(answer) = paste_answer(&o.spec, age, key) else {
            return;
        };
        self.apply_nav_answer(answer);
    }

    /// Button `answer` of the consent or link-confirm bar (key or click).
    fn apply_nav_answer(&mut self, answer: usize) {
        if let Some(c) = self.nav.consent.take() {
            let a = match answer {
                0 => Answer::Once,
                1 => Answer::Always,
                _ => Answer::Deny,
            };
            self.nav.consents.answer(c.pane, c.class, a, Instant::now());
            self.refresh_bar();
            if c.class == Class::GraphicsFile {
                self.graphics_file_answer(c.pane, c.token, a != Answer::Deny);
            } else if a != Answer::Deny {
                self.osc52_read(c.pane, c.selection);
            }
        } else if let Some(p) = self.nav.open.take() {
            self.refresh_bar();
            if answer == 0 {
                self.spawn_open(&p.uri, None);
            }
        }
    }

    fn send_paste(&mut self, id: PaneKey, bytes: &[u8]) {
        let Some(p) = self.panes.get(&id) else { return };
        let modes = p.shared.mirror.load();
        match paste::encode(bytes, modes, &self.settings.input.paste) {
            Ok(unit) => {
                self.typed();
                tracing::debug!(bytes = unit.len(), "paste sent");
                self.send_unit(id, unit);
            }
            Err(paste::Refused::TooLarge) => {
                tracing::warn!(bytes = bytes.len(), "paste refused: over paste.max-bytes");
                self.handle.ring_bell(Some(self.win));
            }
            Err(paste::Refused::Empty) => {}
        }
    }

    /// A key answers the paste confirm bar: Paste (0) or Cancel (1, also Escape).
    fn answer_paste(&mut self, key: apoth_input::Key) {
        let Some(o) = &self.bar else { return };
        let age = o.shown_at.elapsed().as_millis() as u64;
        let Some(answer) = paste_answer(&o.spec, age, key) else {
            return;
        };
        self.apply_paste_answer(answer);
    }

    /// Button `answer` of the paste confirm bar (key or click): Paste (0) or Cancel (1).
    fn apply_paste_answer(&mut self, answer: usize) {
        let Some(pending) = self.paste.take() else {
            return;
        };
        self.refresh_bar();
        if answer == 0 {
            self.send_paste(pending.pane, &pending.bytes);
        }
    }

    /// A click on button `i` of the bar or sheet, whichever question is up. The input guard
    /// was checked by the caller; Return and Space never reach here.
    fn bar_click_answer(&mut self, i: usize) {
        if self.paste.is_some() {
            self.apply_paste_answer(i);
        } else if self.nav.consent.is_some() || self.nav.open.is_some() {
            self.apply_nav_answer(i);
        } else if self.auto.confirm.is_some() {
            self.answer_rule(Some(i));
        } else {
            // The error bar and the restart notice have one button: dismiss.
            self.bar_dismissed = true;
            self.restart_keys.clear();
            self.refresh_bar();
        }
    }

    /// The overlay's pointer handling: the bar's buttons, the rule sheet (modal), and the
    /// search sheet. A control acts on release over the one it went down on; a press inside
    /// the 500 ms input guard does nothing at all (T-18 §3). True when the pointer event was
    /// consumed.
    fn overlay_pointer(&mut self, spot: Spot, pressed: bool, left: bool) -> bool {
        let over = matches!(
            spot,
            Spot::BarButton(_)
                | Spot::BarBody
                | Spot::SheetOutside
                | Spot::Search(_)
                | Spot::SearchSheet
        );
        if pressed {
            if !over {
                return false;
            }
            if left {
                let guarded = matches!(spot, Spot::BarButton(_))
                    && self.bar.as_ref().is_some_and(|o| {
                        o.shown_at.elapsed().as_millis()
                            < u128::from(apoth_ui::tokens::motion::CONSENT_GUARD_MS)
                    });
                if !guarded {
                    self.overlay_down = Some(spot);
                    if let Some(w) = self.spot_widget(spot) {
                        self.ui_state.press(w);
                    }
                    if spot == Spot::Search(SearchTarget::Field) {
                        self.search_focus();
                    }
                    self.send_layout();
                }
            }
            return true;
        }
        let Some(down) = self.overlay_down.take() else {
            return over;
        };
        self.ui_state.release();
        if left && down == spot {
            match spot {
                Spot::BarButton(i) => self.bar_click_answer(i),
                Spot::Search(SearchTarget::Chip(c)) => self.search_chip(c),
                Spot::Search(SearchTarget::Prev) => {
                    self.search_step(false);
                    self.send_layout();
                }
                Spot::Search(SearchTarget::Next) => {
                    self.search_step(true);
                    self.send_layout();
                }
                Spot::Search(SearchTarget::Close) => self.close_search(),
                _ => {}
            }
        }
        self.send_layout();
        true
    }

    /// The search field takes the keyboard (a click on it).
    fn search_focus(&mut self) {
        if let Some(s) = self.nav.search.as_mut() {
            s.focused = true;
        }
    }

    /// Flip one mode chip, exactly as its `Alt` chord does, and search again at once.
    fn search_chip(&mut self, chip: Chip) {
        let Some(s) = self.nav.search.as_mut() else {
            return;
        };
        match chip {
            Chip::Case => {
                s.query.case = match s.query.case {
                    Case::Smart => Case::Sensitive,
                    Case::Sensitive => Case::Insensitive,
                    Case::Insensitive => Case::Smart,
                };
            }
            Chip::Regex => s.query.regex = !s.query.regex,
            Chip::Word => s.query.whole_word = !s.query.whole_word,
            Chip::Scope => {
                s.query.scope = match s.query.scope {
                    crate::search::Scope::All => crate::search::Scope::Screen,
                    crate::search::Scope::Screen => crate::search::Scope::All,
                };
            }
        }
        self.search_now();
    }

    // ---------------------------------------------------------------- pointer

    /// Interface units of a surface-logical position.
    fn ui_point(&self, x: f64, y: f64) -> Option<Point> {
        let g = self.geo?;
        let s = self.scale();
        Some(Point::new((x * s) as f32 / g.ui, (y * s) as f32 / g.ui))
    }

    /// What is under a surface-logical position (T-08 §5 order: sheet, bar, strip, panes).
    fn spot(&self, x: f64, y: f64) -> Spot {
        let (Some(g), Some(pt)) = (self.geo, self.ui_point(x, y)) else {
            return Spot::Nothing;
        };
        if let Some(spec) = self.palette_spec(g.area) {
            let l = apoth_ui::palette::layout(g.area, &spec);
            return match l.row_at(pt) {
                Some(i) => Spot::PaletteRow(i),
                None if l.sheet.contains(pt) => Spot::PaletteSheet,
                None => Spot::PaletteOutside,
            };
        }
        if let Some(o) = &self.bar {
            let l = apoth_ui::bar::layout(g.area, &o.spec, &apoth_ui::text::geometry());
            if !o.takes_room() {
                // A modal sheet owns the pointer over the whole window.
                return match l.button_at(pt) {
                    Some(i) => Spot::BarButton(i),
                    None if l.bar.contains(pt) => Spot::BarBody,
                    None => Spot::SheetOutside,
                };
            }
            if l.bar.contains(pt) {
                return l.button_at(pt).map_or(Spot::BarBody, Spot::BarButton);
            }
        }
        if let Some(sl) = self.search_layout() {
            if let Some(t) = sl.target_at(pt) {
                return Spot::Search(t);
            }
            if sl.sheet.contains(pt) {
                return Spot::SearchSheet;
            }
        }
        if let Some(r) = g.strip
            && r.contains(pt)
        {
            let tabs = self.tab_specs();
            let l = apoth_ui::tabs::layout(r, STRIP_ID, &tabs);
            if l.new_tab.contains(pt) {
                return Spot::NewTab;
            }
            for c in &l.cells {
                if c.close.contains(pt) {
                    return Spot::TabClose(c.index);
                }
                if c.rect.contains(pt) {
                    return Spot::Tab(c.index);
                }
            }
            return Spot::Strip;
        }
        let (px, py) = (x * self.scale(), y * self.scale());
        self.placed
            .iter()
            .find(|p| p.contains(self.cell, px, py))
            .map_or(Spot::Nothing, |p| Spot::Pane(p.id))
    }

    /// The search sheet's layout, when it is open.
    fn search_layout(&self) -> Option<apoth_ui::searchbar::SearchLayout> {
        let g = self.geo?;
        let spec = self.search_spec()?;
        let area = crate::ui::search_area(
            g.area,
            g.strip,
            self.bar.as_ref().is_some_and(Overlay::takes_room),
        );
        Some(apoth_ui::searchbar::layout(
            area,
            &spec,
            &apoth_ui::text::geometry(),
        ))
    }

    /// The widget a spot highlights on hover.
    fn spot_widget(&self, s: Spot) -> Option<WidgetId> {
        let tab = |i: usize| {
            self.session
                .window(self.mux_win)
                .and_then(|w| w.tabs.get(i).copied())
                .map(tab_widget)
        };
        match s {
            Spot::PaletteRow(i) => Some(apoth_ui::palette::row_id(PALETTE_ID, i)),
            Spot::Tab(i) => tab(i),
            Spot::TabClose(i) => tab(i).map(|t| t.child(0)),
            Spot::NewTab => Some(STRIP_ID.child(u64::MAX)),
            Spot::BarButton(i) => self.bar.as_ref().map(|o| o.spec.id.child(i as u64 + 1)),
            Spot::Search(t) => Some(apoth_ui::searchbar::target_id(SEARCH_ID, t)),
            _ => None,
        }
    }

    fn set_hover(&mut self, id: Option<WidgetId>) {
        if self.ui_state.hover != id {
            self.ui_state.set_hover(id);
            self.send_layout();
        }
    }

    /// Geometry of pane `id` for the pointer state machine.
    fn geometry_of(&self, id: PaneKey) -> Option<Geometry> {
        let p = self.panes.get(&id)?;
        Some(Geometry {
            pad: (p.origin.0.max(0) as u32, p.origin.1.max(0) as u32),
            cell: self.cell,
            scale: self.scale(),
            rows: p.dims.0,
            cols: p.dims.1,
        })
    }

    /// The pane pointer gestures go to: the one a held button went down in, else the focused.
    fn gesture_pane(&self) -> Option<PaneKey> {
        self.capture.or(self.mouse_pane).or_else(|| self.focused())
    }

    fn exec(&mut self, id: PaneKey, outs: Vec<Out>) {
        for o in outs {
            match o {
                Out::Pty(b) => self.send_bytes(id, &b),
                Out::View(c) => self.view_of(id, c),
                Out::PastePrimary => self.request_paste(SelectionKind::Primary),
            }
        }
    }

    fn on_pointer_motion(&mut self, x: f64, y: f64) {
        self.pointer = Some((x, y));
        let spot = self.spot(x, y);
        let hover = self.spot_widget(spot);
        self.set_hover(hover);
        if self.palette.is_some() {
            return;
        }
        let target = match (self.capture, spot) {
            (Some(c), _) => c,
            (None, Spot::Pane(id)) => {
                if !self.left_down
                    && self.focused() != Some(id)
                    && self.reloader.config().bool("panes.focus-follows-mouse")
                {
                    self.focus_pane_at(id);
                }
                id
            }
            _ => return,
        };
        let (Some(geo), Some(p)) = (self.geometry_of(target), self.panes.get(&target)) else {
            return;
        };
        let modes = p.shared.mirror.load();
        if self.capture.is_none() && !self.left_down {
            // Hyperlink hover (T-14 §5): the view underlines the link and reports its URI.
            let h = geo.hit(x, y);
            let cell = (!h.above && !h.below).then_some((h.row, h.col));
            if self.nav.hover_cell != Some((target, cell)) {
                self.nav.hover_cell = Some((target, cell));
                self.view_of(target, ViewCmd::Hover(cell));
            }
        }
        let held = self.left_down.then_some(MouseButton::Left);
        let outs = self
            .mouse
            .motion(&geo, x, y, held, self.held, modes, &self.settings.input);
        self.exec(target, outs);
        self.sync_autoscroll();
    }

    /// Run the autoscroll timer exactly while a drag is held past an edge; no timer otherwise.
    fn sync_autoscroll(&mut self) {
        let wanted = self
            .gesture_pane()
            .and_then(|id| self.geometry_of(id))
            .is_some_and(|g| self.mouse.autoscroll_wanted(&g));
        match (wanted, self.autoscroll) {
            (true, None) => {
                self.autoscroll = self
                    .lh
                    .insert_source(
                        Timer::from_duration(AUTOSCROLL_PERIOD),
                        |_, (), app: &mut App| {
                            let target = app.gesture_pane();
                            let geo = target.and_then(|id| app.geometry_of(id));
                            if let (Some(id), Some(g)) = (target, geo) {
                                let outs = app.mouse.autoscroll_tick(&g);
                                app.exec(id, outs);
                                if app.mouse.autoscroll_wanted(&g) {
                                    return TimeoutAction::ToDuration(AUTOSCROLL_PERIOD);
                                }
                            }
                            app.autoscroll = None;
                            TimeoutAction::Drop
                        },
                    )
                    .ok();
            }
            (false, Some(t)) => {
                self.lh.remove(t);
                self.autoscroll = None;
            }
            _ => {}
        }
    }

    fn on_pointer_button(&mut self, code: u32, pressed: bool) {
        const BTN_LEFT: u32 = 0x110;
        const BTN_MIDDLE: u32 = 0x112;
        if code == BTN_LEFT {
            self.left_down = pressed;
        }
        let spot = self.pointer.map_or(Spot::Nothing, |(x, y)| self.spot(x, y));
        // The palette sheet is modal: a row runs on press, a click outside dismisses.
        match spot {
            Spot::PaletteRow(i) => {
                if pressed && code == BTN_LEFT {
                    let window = self.palette_rows();
                    if let Some(p) = self.palette.as_mut() {
                        let at = p.scroll + i;
                        p.select(at, window);
                    }
                    self.palette_pick();
                }
                return;
            }
            Spot::PaletteSheet => return,
            Spot::PaletteOutside => {
                if pressed {
                    self.close_palette();
                }
                return;
            }
            _ => {}
        }
        if self.overlay_pointer(spot, pressed, code == BTN_LEFT) {
            return;
        }
        if self.capture.is_none() {
            // Strip controls act on release over the control they went down on.
            let w = self.spot_widget(spot);
            match spot {
                Spot::Tab(_) | Spot::TabClose(_) | Spot::NewTab | Spot::Strip => {
                    if pressed {
                        if let Some(id) = w {
                            self.ui_state.press(id);
                        }
                        self.send_layout();
                        return;
                    }
                    let was = self.ui_state.release();
                    let same = was.is_some() && was == w;
                    match spot {
                        Spot::Tab(i) if same && code == BTN_LEFT => {
                            self.focus_tab(TabTarget::Index(i));
                        }
                        Spot::Tab(i)
                            if same
                                && code == BTN_MIDDLE
                                && self.reloader.config().bool("tabs.middle-click-close") =>
                        {
                            if let Some(t) = self
                                .session
                                .window(self.mux_win)
                                .and_then(|w| w.tabs.get(i).copied())
                            {
                                self.close_tab(t);
                            }
                        }
                        Spot::TabClose(i) if same && code == BTN_LEFT => {
                            if let Some(t) = self
                                .session
                                .window(self.mux_win)
                                .and_then(|w| w.tabs.get(i).copied())
                            {
                                self.close_tab(t);
                            }
                        }
                        Spot::NewTab if same && code == BTN_LEFT => self.new_tab(None, None),
                        _ => {}
                    }
                    self.send_layout();
                    return;
                }
                _ => {}
            }
        }
        let target = if pressed {
            let Spot::Pane(id) = spot else { return };
            // A press focuses the pane under it (T-13 §4) and holds it for the gesture.
            self.focus_pane_at(id);
            if self.mouse_pane.is_some_and(|m| m != id) {
                // One selection per window: the old pane's goes when a new gesture starts.
                if let Some(old) = self.mouse_pane {
                    self.view_of(old, ViewCmd::Clear);
                }
                self.mouse.selection_gone();
            }
            if code == BTN_LEFT && self.open_modifier_held() {
                // Open-modifier click activates the link under the pointer (T-14 §5).
                if let (Some(geo), Some((x, y))) = (self.geometry_of(id), self.pointer) {
                    let h = geo.hit(x, y);
                    self.view_of(
                        id,
                        ViewCmd::Activate {
                            row: h.row,
                            col: h.col,
                        },
                    );
                }
            }
            self.capture = Some(id);
            self.mouse_pane = Some(id);
            if self.mouse_bind(code, true) {
                return;
            }
            id
        } else {
            let Some(id) = self.capture.take().or(self.mouse_pane) else {
                return;
            };
            if self.mouse_bind(code, false) {
                return;
            }
            id
        };
        let (Some(geo), Some(p)) = (self.geometry_of(target), self.panes.get(&target)) else {
            return;
        };
        let modes = p.shared.mirror.load();
        let outs = self.mouse.button(
            &geo,
            code,
            pressed,
            self.held,
            modes,
            Instant::now(),
            &self.settings.input,
        );
        self.exec(target, outs);
        self.sync_autoscroll();
    }

    /// Mouse binds (T-11 §3): exact modifiers, press or release. None are bound by default.
    fn mouse_bind(&mut self, code: u32, pressed: bool) -> bool {
        use apoth_action::key::{MouseButton as B, MouseOn};
        if self.keymap.mouse.is_empty() {
            return false;
        }
        let button = match code {
            0x110 => B::Left,
            0x111 => B::Right,
            0x112 => B::Middle,
            0x113 => B::Back,
            0x114 => B::Forward,
            _ => return false,
        };
        let Some(mods) = binds::bind_mods(self.held) else {
            return false;
        };
        let on = if pressed {
            MouseOn::Press
        } else {
            MouseOn::Release
        };
        let Some(actions) = binds::mouse_lookup(&self.keymap, mods, button, on) else {
            return false;
        };
        let actions = actions.to_vec();
        self.dispatch(actions, 0);
        true
    }

    fn on_scroll(&mut self, lines_y: i32, lines_x: i32) {
        let spot = self.pointer.map_or(Spot::Nothing, |(x, y)| self.spot(x, y));
        match spot {
            Spot::PaletteRow(_) | Spot::PaletteSheet | Spot::PaletteOutside => {
                let window = self.palette_rows();
                if let Some(p) = self.palette.as_mut() {
                    p.move_by(lines_y.signum(), window);
                }
                self.send_layout();
                return;
            }
            Spot::Tab(_) | Spot::TabClose(_) | Spot::NewTab | Spot::Strip => {
                if lines_y != 0 && self.reloader.config().bool("tabs.wheel-switch") {
                    let (cur, n) = self.session.window(self.mux_win).map_or((0, 0), |w| {
                        let cur = w
                            .active
                            .and_then(|a| w.tabs.iter().position(|t| *t == a))
                            .unwrap_or(0);
                        (cur, w.tabs.len())
                    });
                    let to = apoth_ui::tabs::wheel_switch(cur, lines_y.signum(), n);
                    self.focus_tab(TabTarget::Index(to));
                }
                return;
            }
            _ => {}
        }
        if matches!(spot, Spot::Pane(_)) && lines_y != 0 && self.keymap_wheel(lines_y) {
            return;
        }
        let target = match spot {
            Spot::Pane(id) => id,
            _ => match self.focused() {
                Some(id) => id,
                None => return,
            },
        };
        let (Some(geo), Some(p)) = (self.geometry_of(target), self.panes.get(&target)) else {
            return;
        };
        let modes = p.shared.mirror.load();
        let outs = self.mouse.scroll(
            &geo,
            lines_y,
            lines_x,
            self.held,
            modes,
            &self.settings.input,
        );
        self.exec(target, outs);
    }

    /// Wheel mouse binds (`ctrl+wheel-up` and the like).
    fn keymap_wheel(&mut self, lines_y: i32) -> bool {
        use apoth_action::key::{MouseButton as B, MouseOn};
        if self.keymap.mouse.is_empty() {
            return false;
        }
        let Some(mods) = binds::bind_mods(self.held) else {
            return false;
        };
        // Positive lines scroll back (up).
        let button = if lines_y > 0 {
            B::WheelUp
        } else {
            B::WheelDown
        };
        let Some(actions) = binds::mouse_lookup(&self.keymap, mods, button, MouseOn::Press) else {
            return false;
        };
        let actions = actions.to_vec();
        self.dispatch(actions, 0);
        true
    }

    // -------------------------------------------------------------------- IME

    fn on_ime(&mut self, e: ImeEvent) {
        match e {
            ImeEvent::Enter { .. } => {
                if let Some(seat) = self.seat {
                    // The rectangle first: the seat caches it and sends it with the enable.
                    self.ime_send_rect();
                    self.handle.ime_enable(seat);
                    self.ime_on = true;
                }
            }
            ImeEvent::Leave { .. } => {
                if let Some(seat) = self.seat {
                    self.handle.ime_disable(seat);
                }
                self.ime_on = false;
                self.view(ViewCmd::Preedit(None));
            }
            ImeEvent::Preedit {
                text,
                cursor_begin,
                cursor_end,
            } => {
                // A paste confirm or the palette owns the keyboard: no composition shows.
                let owned = self.paste.is_some() || self.palette.is_some();
                let text = text.filter(|_| !owned);
                let p = text.filter(|t| !t.0.is_empty()).map(|t| Preedit {
                    text: t.0,
                    begin: cursor_begin,
                    end: cursor_end,
                });
                self.view(ViewCmd::Preedit(p));
            }
            ImeEvent::Commit(t) => {
                if self.palette.is_some() {
                    // Composed text goes into the palette query (never logged).
                    if let Some(p) = self.palette.as_mut()
                        && p.type_text(&t.0)
                    {
                        self.palette_changed();
                    }
                    return;
                }
                // Committed text is user content: encoded (kitty flag 16 aware) and written,
                // never logged. Control characters are dropped by the encoder.
                let Some(id) = self.focused() else { return };
                let Some(p) = self.panes.get(&id) else { return };
                let out = commit_bytes(self.paste.is_some(), &t.0, p.shared.mirror.load());
                if !out.is_empty() {
                    self.typed();
                    self.send_unit(id, out);
                }
            }
            ImeEvent::DeleteSurrounding { .. } => {}
        }
    }

    /// The cursor moved: remember it and report the rectangle.
    fn ime_cursor(&mut self, cell: (usize, usize)) {
        self.cursor_cell = cell;
        self.ime_send_rect();
    }

    /// Report the focused pane's cursor rectangle in surface-logical pixels (text-input-v3).
    /// Called on every cursor move, on enable, and after every geometry change. The seat
    /// caches it and only sends it while the text input is enabled.
    fn ime_send_rect(&mut self) {
        let Some(seat) = self.seat else { return };
        let Some(geo) = self.focused().and_then(|id| self.geometry_of(id)) else {
            return;
        };
        let (row, col) = self.cursor_cell;
        let (cw, ch) = (f64::from(geo.cell.0), f64::from(geo.cell.1));
        let x = (f64::from(geo.pad.0) + col as f64 * cw) / geo.scale;
        let y = (f64::from(geo.pad.1) + row as f64 * ch) / geo.scale;
        self.handle.ime_set_cursor_rect(
            seat,
            Rect {
                x: x.round() as i32,
                y: y.round() as i32,
                w: (cw / geo.scale).ceil() as i32,
                h: (ch / geo.scale).ceil() as i32,
            },
        );
    }

    // ---------------------------------------------------------------- pty write

    /// Key and focus bytes: written straight to the master when nothing is queued (no
    /// allocation on the key to PTY path). Only a short write or a busy queue falls back to
    /// the owned-unit queue of `apoth-pty`, whose `send` takes a `Vec` (an API gap outside
    /// this crate; the slow path allocates, the common path does not).
    fn send_bytes(&mut self, id: PaneKey, bytes: &[u8]) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        let Some(pty) = p.pty.as_mut() else { return };
        if !pty.wants_pollout() {
            match rustix::io::write(pty.master_fd(), bytes) {
                Ok(n) if n == bytes.len() => return,
                Ok(n) => return self.send_unit(id, bytes[n..].to_vec()),
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(e) => {
                    tracing::warn!(errno = e.raw_os_error(), "pty write failed");
                    return;
                }
            }
        }
        self.send_unit(id, bytes.to_vec());
    }

    fn send_unit(&mut self, id: PaneKey, unit: Vec<u8>) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        let Some(pty) = p.pty.as_mut() else { return };
        match pty.send(unit) {
            Ok(Enqueue::Written) => {}
            Ok(Enqueue::Queued) => self.arm_pollout(id),
            Ok(Enqueue::Dropped) => {
                tracing::warn!(
                    dropped = pty.dropped_units(),
                    "outbound queue full; unit dropped"
                );
            }
            Err(e) => tracing::warn!(kind = ?e.kind(), "pty write failed"),
        }
    }

    fn arm_pollout(&mut self, id: PaneKey) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        if p.pollout.is_some() {
            return;
        }
        let Some(pty) = p.pty.as_ref() else { return };
        let Ok(fd) = pty.master_fd().try_clone_to_owned() else {
            return;
        };
        match self.lh.insert_source(
            Generic::new(fd, Interest::WRITE, Mode::Level),
            move |_, _, app: &mut App| Ok(app.on_pollout(id)),
        ) {
            Ok(t) => p.pollout = Some(t),
            Err(e) => tracing::warn!(error = %e.error, "cannot watch the pty for writes"),
        }
    }

    fn on_pollout(&mut self, id: PaneKey) -> PostAction {
        let Some(p) = self.panes.get_mut(&id) else {
            return PostAction::Remove;
        };
        let Some(pty) = p.pty.as_mut() else {
            p.pollout = None;
            return PostAction::Remove;
        };
        match pty.drain_out() {
            Ok(false) => PostAction::Continue,
            Ok(true) | Err(_) => {
                p.pollout = None;
                PostAction::Remove
            }
        }
    }

    // ------------------------------------------------------------ pane messages

    fn on_msg(&mut self, m: MainMsg) {
        match m {
            MainMsg::Reply(id, u) => self.send_unit(id, u),
            MainMsg::Abyss(a) => self.on_abyss(a),
            MainMsg::Ctl(cid, r) => self.ctl_send(cid, &r),
            MainMsg::A11y(m) => self.on_a11y(m),
            MainMsg::Resized(id, d) => self.on_resized(id, d),
            MainMsg::Events(id, e) => self.on_events(id, e),
            MainMsg::Eof(id) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.eof = true;
                }
                self.maybe_finish(id);
            }
            MainMsg::RenderFailed(why) => {
                tracing::error!(%why, "rendering is unavailable");
                eprintln!("apotheosis: cannot render: {why}");
                self.exit_code = 69;
                self.begin_close();
            }
            MainMsg::CellSize(c) => {
                self.cell = c;
                self.relayout();
            }
            MainMsg::Copied(c) => self.on_copied(c),
            MainMsg::Nav(id, m) => self.on_nav(id, m),
            MainMsg::Cursor(id, c) => {
                if self.focused() == Some(id) {
                    self.ime_cursor(c);
                }
            }
            MainMsg::Loaded(r) => self.on_loaded(*r),
            MainMsg::Closed(id, status) => {
                self.closing = self.closing.saturating_sub(1);
                if let Some(i) = self.forced.iter().position(|f| *f == id) {
                    self.forced.swap_remove(i);
                    if self.panes.is_empty() && self.exit_code == 0 {
                        // A close we asked for is a clean exit whatever signal ended the child.
                        self.exit_code = match status {
                            Some(ExitStatus::Code(c)) => c.min(255),
                            _ => 0,
                        };
                    }
                }
                self.maybe_quit();
            }
        }
    }

    /// `bell.visual`: a bell in a tab out of sight badges the tab; in the shown tab the
    /// pane's rim brightens for a moment. Nothing moves; the rim goes out on a timer, so a
    /// settled window asks for no frames.
    fn visual_bell(&mut self, id: PaneKey) {
        if !self.reloader.config().bool("bell.visual") {
            return;
        }
        let tab = self.session.tab_of(id);
        if tab != self.active_tab() {
            if let Some(t) = tab {
                self.status.bell_tabs.insert(t);
            }
            self.send_layout();
            return;
        }
        if let Some((_, t)) = self.status.bell_flash.take() {
            self.lh.remove(t);
        }
        let timer =
            self.lh
                .insert_source(Timer::from_duration(BELL_FLASH), |_, (), app: &mut App| {
                    app.status.bell_flash = None;
                    app.send_layout();
                    TimeoutAction::Drop
                });
        if let Ok(t) = timer {
            self.status.bell_flash = Some((id, t));
        }
        self.send_layout();
    }

    fn on_events(&mut self, id: PaneKey, mut e: PaneEvents) {
        let uri = e.cwd.clone();
        if let Some(uri) = e.cwd.take() {
            let path = crate::ctl::osc7::local_path(&uri, &self.hostname);
            if let Some(p) = self.panes.get_mut(&id) {
                p.cwd = path.clone();
            }
            let mut ev = Event::new(EventKind::CwdChanged, Cause::Sequence);
            if let Some(path) = path {
                let _ = ev.set("cwd", FieldValue::Str(path.to_string_lossy().into_owned()));
            }
            self.ctl_emit(ev, Some(id));
        }
        if e.bell > 0 {
            self.visual_bell(id);
            self.ctl_emit(Event::new(EventKind::Bell, Cause::Child), Some(id));
            self.a11y_announce("Bell");
            self.bell_notification(id);
        }
        if let Some(t) = e.title.take() {
            let t = clip(&t, MAX_TITLE).to_owned();
            let focused = self.focused() == Some(id);
            if let Some(p) = self.panes.get_mut(&id) {
                p.title = t.clone();
            }
            if focused {
                self.term_title = t.clone();
                let title = self.window_title_now();
                self.handle.set_title(self.win, &title);
            }
            if self.geo.is_some_and(|g| g.strip.is_some()) {
                self.send_layout();
            }
            let mut ev = Event::new(EventKind::TitleChanged, Cause::Sequence);
            let _ = ev.set("title", FieldValue::Str(t));
            self.ctl_emit(ev, Some(id));
        }
        if e.bell > 0 && self.settings.bell_audible {
            self.handle.ring_bell(Some(self.win));
        }
        if !e.clipboard.is_empty() {
            self.on_osc52_write(id, std::mem::take(&mut e.clipboard));
        }
        if let Some(sel) = e.clip_read.take() {
            self.on_osc52_read(id, sel);
        }
        if let Some(token) = e.graphics_file.take() {
            self.on_graphics_file(id, token);
        }
        self.auto_pane_events(id, &mut e, uri.as_deref());
    }

    // --------------------------------------------------------------- lifecycle

    fn on_child_exit(&mut self, id: PaneKey) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        let Some(pty) = p.pty.as_mut() else { return };
        match pty.try_wait() {
            Ok(Some(st)) => {
                p.exit = Some(st);
                tracing::info!(pane = id, code = exit_code_of(st).min(255), "child exited");
            }
            _ => return,
        }
        if p.grace.is_none() {
            // Wait a moment for the last output before the pane goes.
            match self.lh.insert_source(
                Timer::from_duration(EXIT_GRACE),
                move |_, (), app: &mut App| {
                    if let Some(p) = app.panes.get_mut(&id) {
                        p.grace = None;
                        p.eof = true;
                    }
                    app.maybe_finish(id);
                    TimeoutAction::Drop
                },
            ) {
                Ok(t) => p.grace = Some(t),
                Err(_) => p.eof = true,
            }
        }
        self.maybe_finish(id);
    }

    /// A pane whose child exited and whose output is drained goes (`panes.remain-on-exit` is
    /// not implemented yet: reported gap).
    fn maybe_finish(&mut self, id: PaneKey) {
        if self
            .panes
            .get(&id)
            .is_some_and(|p| p.exit.is_some() && p.eof)
        {
            self.retire(id);
        }
    }

    fn shutdown(&mut self) {
        let panes = std::mem::take(&mut self.panes);
        for (_, mut p) in panes {
            if let Some(io) = p.io.take() {
                let _ = rustix::io::write(&io.wake, &[WAKE_STOP]);
                let _ = io.join.join();
            }
        }
        if let Some(tx) = &self.render_tx {
            let _ = tx.send(RenderMsg::Stop);
        }
        if let Some(r) = self.render.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !r.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if r.is_finished() {
                let _ = r.join();
            }
        }
        self.handle.destroy_window(self.win);
        self.handle.flush();
    }

    // ------------------------------------------------------------------ hot reload

    /// Watch the config files' directories on the loop; where inotify cannot be set up, poll
    /// every 2 s instead (reported by `apoth doctor`, excluded from the T-23 idle check).
    fn start_watch(&mut self, files: &[std::path::PathBuf]) {
        let mut w = match Watcher::new() {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(
                    errno = e.raw_os_error(),
                    "inotify unavailable; polling config"
                );
                return self.start_poll();
            }
        };
        let failed = w.sync(files);
        if w.watched() == 0 && failed > 0 {
            tracing::warn!("no config directory can be watched; polling config");
            return self.start_poll();
        }
        let registered = w.poll_fd().map_err(|e| e.to_string()).and_then(|fd| {
            self.lh
                .insert_source(
                    Generic::new(fd, Interest::READ, Mode::Level),
                    |_, _, app: &mut App| {
                        app.on_config_event();
                        Ok(PostAction::Continue)
                    },
                )
                .map_err(|e| e.error.to_string())
        });
        match registered {
            Ok(_) => {
                tracing::info!(dirs = w.watched(), "watching the configuration");
                self.watcher = Some(w);
                self.watch_mode = WatchMode::Inotify;
            }
            Err(why) => {
                tracing::warn!(%why, "cannot watch the configuration; polling");
                self.start_poll();
            }
        }
    }

    fn start_poll(&mut self) {
        self.watch_mode = WatchMode::Poll;
        let _ = self
            .lh
            .insert_source(Timer::from_duration(POLL_PERIOD), |_, (), app: &mut App| {
                app.start_reload();
                TimeoutAction::ToDuration(POLL_PERIOD)
            });
    }

    fn on_config_event(&mut self) {
        let any = self.watcher.as_ref().is_some_and(Watcher::drain);
        if !any {
            return;
        }
        if let Some(t) = self.debounce.take() {
            self.lh.remove(t);
        }
        match self
            .lh
            .insert_source(Timer::from_duration(DEBOUNCE), |_, (), app: &mut App| {
                app.debounce = None;
                app.start_reload();
                TimeoutAction::Drop
            }) {
            Ok(t) => self.debounce = Some(t),
            Err(_) => self.start_reload(),
        }
    }

    /// Parse on a worker (file I/O never runs on the main loop); the result comes back as
    /// `MainMsg::Loaded`. One load at a time: events during one queue exactly one more.
    fn start_reload(&mut self) {
        if self.loading {
            self.reload_queued = true;
            return;
        }
        self.loading = true;
        // The worker owns a copy of the live reloader: its baseline is the last applied config.
        let mut rl = self.reloader.clone();
        let launch = self.launch.clone();
        let tx = self.to_main.clone();
        let job = move || {
            let input = load_input(&launch);
            let out = rl.reload(&RealFs, &input);
            let _ = tx.send(MainMsg::Loaded(Box::new((rl, out))));
        };
        if let Err(job) = self.workers.submit(Box::new(job)) {
            // Queue full or no worker: run it here rather than lose the reload.
            job();
        }
    }

    fn on_loaded(&mut self, r: (Reloader, Outcome)) {
        self.loading = false;
        let (rl, outcome) = r;
        self.reloader = rl;
        if let Some(w) = self.watcher.as_mut() {
            // A new include extends the watch set.
            w.sync(self.reloader.files());
        }
        let mut ev = Event::new(EventKind::ConfigReloaded, Cause::Config);
        let _ = ev.set(
            "ok",
            FieldValue::Bool(!matches!(outcome, Outcome::Rejected)),
        );
        self.ctl_emit(ev, None);
        self.a11y_reconfigure();
        let mut errors = error_positions(self.reloader.diagnostics());
        if matches!(outcome, Outcome::Rejected) && errors.is_empty() {
            errors.push(ErrorPos {
                file: None,
                line: 0,
                column: 0,
                key: None,
                message: String::new(),
            });
        }
        if !errors.is_empty() && errors != self.config_errors {
            for e in errors.iter().take(20) {
                tracing::warn!(
                    file = ?e.file,
                    line = e.line,
                    column = e.column,
                    key = e.key.as_deref().unwrap_or(""),
                    "configuration error"
                );
            }
            tracing::warn!(
                errors = errors.len(),
                rejected = matches!(outcome, Outcome::Rejected),
                "configuration has errors; bad entries keep their last good value"
            );
        }
        if let Outcome::Applied(a) = &outcome {
            let mut settings = Settings::from_config(self.reloader.config());
            settings.diagnostics = self.reloader.diagnostics().len();
            tracing::info!(
                keys = a.diff.keys.len(),
                collections = a.diff.collections.len(),
                "configuration reloaded"
            );
            self.apply_applied(settings, a);
            errors.extend(error_positions(&self.recompile_rules()));
        }
        self.set_config_errors(errors);
        // The queued reload starts only now, from the config just applied: its baseline is the
        // last good value for any key a bad scalar leaves alone (T-10 §4).
        if std::mem::take(&mut self.reload_queued) {
            self.start_reload();
        }
    }

    /// Tell each subsystem what changed. Every settings reader below the loop either gets a
    /// message (render) or reads `self.settings` at use (bell, shell, title).
    fn apply_applied(&mut self, settings: Settings, a: &Applied) {
        let f = flags_of(a);
        let old_padding = self.settings.padding;
        self.settings = settings;
        self.settings.apply_theme(&self.theme_colors());
        self.push_term_colors();
        if f.render {
            self.send_render_update(f.font);
        }
        let ids: Vec<PaneKey> = self.panes.keys().copied().collect();
        let deny = self.deny_word();
        for id in ids {
            self.send_view_settings(id);
            if let Some(p) = self.panes.get(&id) {
                p.shared.deny.store(deny, Ordering::Release);
            }
        }
        if f.title {
            let title = self.window_title_now();
            self.handle.set_title(self.win, &title);
        }
        // Padding, tabs.* and panes.* all move the geometry or the chrome: lay out again.
        let _ = old_padding;
        self.relayout();
        if f.keys {
            self.keymap = Keymap::from_config(self.reloader.config());
            self.keys.reset();
            tracing::info!(dropped = self.keymap.dropped, "key bindings changed");
            let tabs = self.palette_tabs();
            if let Some(p) = self.palette.as_mut() {
                p.refresh(&self.keymap, &tabs);
            }
        }
        if f.term {
            // HOOK(apoth-term): the running Term has no config setter yet (scrollback,
            // OSC 52, default cursor, sync timeout in the io thread); these keys take effect
            // for the next pane. The render-side sync timeout and echo window are live.
            tracing::debug!("term-owned keys changed; they apply to new panes");
        }
        let mut new_restart = false;
        for (path, class) in &a.deferred {
            if *class == Reload::Restart {
                if !self.restart_keys.contains(path) {
                    self.restart_keys.push(path);
                    new_restart = true;
                }
            } else {
                tracing::info!(key = path, "change applies to new panes or windows");
            }
        }
        if new_restart {
            tracing::info!(keys = ?self.restart_keys, "restart required for some changes");
            self.bar_dismissed = false;
            self.refresh_bar();
        }
    }

    // ------------------------------------------------------------------ theme (T-17)

    /// Detect Abyss on a thread of its own; it reports here (T-17 §1).
    fn start_abyss(&mut self) {
        let tx = self.to_main.clone();
        if let Err(e) = apoth_eclipse::detect::spawn(move |a| {
            let _ = tx.send(MainMsg::Abyss(a));
        }) {
            tracing::warn!(error = %e, "could not start Abyss detection");
        }
    }

    /// The theme colours in force: Abyss's under `theme "system"`, else the stock scheme.
    fn theme_colors(&self) -> apoth_eclipse::TermColors {
        match self.abyss_theme {
            Some(t) if self.settings.theme_system => t,
            _ => apoth_eclipse::TermColors::stock(true),
        }
    }

    /// Abyss came, changed its theme, or went: re-derive, re-check, repaint, and let the
    /// panes' programs know (mode 2031). A theme that fails the contrast check is refused
    /// and the colours in force stay.
    fn on_abyss(&mut self, a: apoth_eclipse::Abyss) {
        self.abyss_theme = match a {
            apoth_eclipse::Abyss::Theme(seeds) => {
                match apoth_eclipse::TermColors::checked(&seeds) {
                    Ok(t) => Some(t),
                    Err(e) => {
                        tracing::warn!(error = ?e, "Abyss theme refused: it fails the contrast check");
                        return;
                    }
                }
            }
            apoth_eclipse::Abyss::Gone => None,
        };
        let colours = |s: &crate::settings::Settings| {
            let t = &s.term;
            (
                t.fg,
                t.bg,
                t.cursor_color,
                t.ansi,
                t.dark,
                s.cursor_text,
                s.selection,
            )
        };
        let before = colours(&self.settings);
        self.settings.apply_theme(&self.theme_colors());
        if colours(&self.settings) == before {
            return;
        }
        self.push_term_colors();
        self.send_render_update(false);
    }

    /// Hand the default colours in force to every running term (OSC queries, OSC 104/110
    /// resets and the mode-2031 report follow them; T-03), and send any report it queued.
    fn push_term_colors(&mut self) {
        let t = &self.settings.term;
        let (fg, bg, cursor, ansi, dark) = (t.fg, t.bg, t.cursor_color, t.ansi, t.dark);
        let ids: Vec<PaneKey> = self.panes.keys().copied().collect();
        for id in ids {
            let mut units = Vec::new();
            {
                let mut term = self.panes[&id].shared.lock();
                term.set_default_colors(fg, bg, cursor, ansi, dark);
                while let Some(u) = term.replies().pop() {
                    units.push(u);
                }
            }
            for u in units {
                self.send_unit(id, u);
            }
        }
    }

    /// Settings the render thread reads, with the font when `font.*` changed.
    fn send_render_update(&mut self, font: bool) {
        let settings = RenderSettings::from_settings(&self.settings, self.blur);
        let font = font.then(|| self.font_config());
        if let Some(tx) = &self.render_tx {
            let _ = tx.send(RenderMsg::Update(Box::new(RenderUpdate { settings, font })));
        }
    }

    /// The font configuration for a font reload: the file's, with the auto-chosen regular
    /// family when the file names none.
    fn font_config(&self) -> apoth_font::config::FontConfig {
        let mut f = self.settings.font.clone();
        if !self.settings.family_named {
            f.styles[Style::Regular.index()].family = self.auto_family.clone();
        }
        f
    }

    /// Record the errors of the last load and show or clear the error bar in every window
    /// (T-10 §4, T-08 §4 R1). Positions and messages only: never the offending text.
    fn set_config_errors(&mut self, errors: Vec<ErrorPos>) {
        if errors.is_empty() && !self.config_errors.is_empty() {
            tracing::info!("configuration is valid again");
        }
        let changed = errors != self.config_errors;
        self.config_errors = errors;
        if changed {
            self.bar_dismissed = false;
            self.refresh_bar();
        }
    }

    /// The one window's widget id; the binary opens a single window (`window.new` is not built).
    fn windows(&self) -> [WidgetId; 1] {
        [WidgetId(1)]
    }

    /// Rebuild the overlay from the current errors and restart keys, tell the render thread,
    /// and make room for it (or give the room back).
    fn refresh_bar(&mut self) {
        let spec = if let Some(pp) = &self.paste {
            Some(apoth_ui::bar::BarSpec::paste_confirm(
                self.windows()[0],
                pp.stats.bytes,
                pp.stats.lines,
                &self.term_title,
            ))
        } else if let Some(c) = &self.nav.consent {
            let title = self
                .panes
                .get(&c.pane)
                .map_or(String::new(), |p| p.title.clone());
            Some(apoth_ui::bar::BarSpec::consent(
                self.windows()[0],
                c.class.label(),
                &title,
            ))
        } else if let Some(o) = &self.nav.open {
            Some(open_bar(self.windows()[0], &o.uri))
        } else if self.auto.confirm.is_some() {
            self.rule_bar()
        } else if self.bar_dismissed {
            None
        } else {
            bars_for(&self.windows(), &self.config_errors, &self.restart_keys)
                .into_iter()
                .find_map(|(_, b)| b)
        };
        let had = self.bar.as_ref().is_some_and(Overlay::takes_room);
        self.bar = spec.map(|spec| Overlay {
            spec,
            shown_at: Instant::now(),
        });
        if let Some(o) = &self.bar {
            // The question is read out once, from interface text only (never the program's).
            let head = o.spec.headline.text.clone();
            self.a11y_announce(&head);
        }
        self.send_overlay();
        if had != self.bar.as_ref().is_some_and(Overlay::takes_room) {
            self.relayout();
        } else {
            self.send_layout();
        }
        if self.bar.is_some() {
            // Redraw once the 500 ms input guard ends so the buttons stop looking inert.
            let guard = Duration::from_millis(apoth_ui::tokens::motion::CONSENT_GUARD_MS + 20);
            let _ = self
                .lh
                .insert_source(Timer::from_duration(guard), |_, (), app: &mut App| {
                    app.send_overlay();
                    TimeoutAction::Drop
                });
        }
    }

    fn send_overlay(&self) {
        if let Some(tx) = &self.render_tx {
            let _ = tx.send(RenderMsg::Overlay(self.bar.clone().map(Box::new)));
        }
    }

    /// Escape answers the bar's dismiss button once its input guard has passed. True when the
    /// key was consumed (never forwarded to the shell).
    fn try_dismiss_bar(&mut self) -> bool {
        let Some(o) = &self.bar else { return false };
        let age = o.shown_at.elapsed().as_millis() as u64;
        let st = apoth_ui::bar::BarState { shown_at_ms: 0 };
        if st
            .answer_key(&o.spec, age, apoth_ui::bar::KeyInput::Escape)
            .is_none()
        {
            return false;
        }
        self.bar_dismissed = true;
        self.restart_keys.clear();
        self.refresh_bar();
        true
    }

    // ------------------------------------------------------------------ resize

    /// Ask for pane `id` to have grid `want`; requests coalesce for 25 ms of quiet.
    fn request_resize(&mut self, id: PaneKey, want: (usize, usize)) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        if want == p.dims && p.pending.is_none() && p.inflight.is_none() {
            return;
        }
        if p.pending == Some(want) || (p.pending.is_none() && p.inflight == Some(want)) {
            return;
        }
        p.pending = Some(want);
        if let Some(t) = self.resize_timer.take() {
            self.lh.remove(t);
        }
        match self
            .lh
            .insert_source(Timer::from_duration(RESIZE_IDLE), |_, (), app: &mut App| {
                app.resize_timer = None;
                app.apply_resizes();
                TimeoutAction::Drop
            }) {
            Ok(t) => self.resize_timer = Some(t),
            Err(_) => self.apply_resizes(),
        }
    }

    /// Hand each pending resize to its pane's io thread, which applies `Term::resize` (a reflow
    /// can be long) under its own lock hold; main never takes the term lock. The size comes back
    /// as `MainMsg::Resized`, then `TIOCSWINSZ` follows.
    fn apply_resizes(&mut self) {
        let cell = self.cell;
        for p in self.panes.values_mut() {
            let Some((rows, cols)) = p.pending.take() else {
                continue;
            };
            let Some(io) = p.io.as_ref() else { continue };
            p.shared.request_resize(ResizeReq { rows, cols, cell });
            p.inflight = Some((rows, cols));
            let _ = rustix::io::write(&io.wake, &[WAKE_RESIZE]);
        }
    }

    fn on_resized(&mut self, id: PaneKey, (nr, nc): (usize, usize)) {
        let cell = self.cell;
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        p.inflight = None;
        p.dims = (nr, nc);
        if let Some(pty) = p.pty.as_mut() {
            let size = PtySize::from_cells(nr as u16, nc as u16, cell.0, cell.1);
            if let Err(e) = pty.resize(size) {
                tracing::warn!(kind = ?e.kind(), "TIOCSWINSZ failed");
            }
        }
        tracing::debug!(pane = id, rows = nr, cols = nc, "resized");
        self.write_mux_dump();
        if self.focused() == Some(id) {
            self.ime_send_rect();
        }
    }
}

/// Which subsystems a reload's live keys touch, from the shared planner's `Applied`.
#[derive(Debug, Default, PartialEq, Eq)]
struct Flags {
    font: bool,
    render: bool,
    title: bool,
    keys: bool,
    term: bool,
}

fn flags_of(a: &Applied) -> Flags {
    let live = |s: Subsystem| a.keys_in(s).any(|k| k.reload == Reload::Live);
    let window = |f: &dyn Fn(&str) -> bool| {
        a.keys_in(Subsystem::Window)
            .any(|k| k.reload == Reload::Live && f(k.path))
    };
    let font = live(Subsystem::Font);
    Flags {
        font,
        render: font
            || live(Subsystem::Render)
            || live(Subsystem::Appearance)
            || window(&|p| {
                p == "window.opacity"
                    || p == "window.blur-fallback"
                    || p == "window.alpha-mode"
                    || p.starts_with("window.padding-")
            }),
        title: window(&|p| p == "window.title"),
        keys: a.touches(Subsystem::Keys),
        term: live(Subsystem::Term)
            || live(Subsystem::Security)
            || a.keys_in(Subsystem::Render)
                .any(|k| k.path == "render.sync-timeout"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(path: &str, v: apoth_config::Value) -> Applied {
        let old = apoth_config::Config::defaults();
        let mut new = old.clone();
        assert!(new.set_at(path, v, apoth_config::model::Origin::builtin()));
        apoth_config::reload::plan(apoth_config::diff(&old, &new), Vec::new())
    }

    #[test]
    fn applied_maps_to_the_subsystems_that_are_told() {
        use apoth_config::Value;
        let f = flags_of(&applied("font.size", Value::Float(15.0)));
        assert!(f.font && f.render && !f.title && !f.keys);
        let f = flags_of(&applied("render.sync-timeout", Value::Int(500)));
        assert!(f.render && f.term && !f.font);
        assert!(flags_of(&applied("window.title", Value::Str("x {title}".into()))).title);
        assert!(flags_of(&applied("window.padding-x", Value::Int(10))).render);
        assert!(flags_of(&applied("window.opacity", Value::Float(0.5))).render);
        let d = applied("window.decorations", Value::Str("none".into()));
        assert_eq!(
            flags_of(&d),
            Flags::default(),
            "new-window keys are not live"
        );
        assert!(!d.deferred.is_empty());
    }

    #[test]
    fn restart_class_keys_are_reported_for_the_notice() {
        // The notice is fed from `Applied::deferred` entries of class Restart.
        let a = applied("render.allow-software", apoth_config::Value::Bool(true));
        let restart: Vec<_> = a
            .deferred
            .iter()
            .filter(|(_, c)| *c == Reload::Restart)
            .collect();
        assert_eq!(a.restart_required, !restart.is_empty());
    }

    #[test]
    fn grid_is_whole_cells_inside_padding() {
        assert_eq!(grid_for((646, 400), (6, 6), (8, 16)), (24, 79));
        assert_eq!(grid_for((0, 0), (6, 6), (8, 16)), (1, 1));
        assert_eq!(
            grid_for((100_000, 100_000), (0, 0), (1, 1)),
            (MAX_ROWS, MAX_COLS)
        );
    }

    #[test]
    fn family_choice_prefers_known_names() {
        let rows = vec![
            ("Zed Mono".to_owned(), true, false),
            ("DejaVu Sans Mono".to_owned(), true, false),
            ("Arial".to_owned(), false, false),
        ];
        assert_eq!(choose_family(&rows).as_deref(), Some("DejaVu Sans Mono"));
        let rows = vec![
            ("Zed Mono".to_owned(), true, false),
            ("Arial".to_owned(), false, false),
        ];
        assert_eq!(choose_family(&rows).as_deref(), Some("Zed Mono"));
        assert_eq!(choose_family(&[("A".into(), false, false)]), None);
    }

    #[test]
    fn app_id_override_rules() {
        let d = "org.eclipseos.Apotheosis";
        assert_eq!(resolve_app_id(None, true), d);
        assert_eq!(resolve_app_id(Some("evil.App"), true), d);
        assert_eq!(resolve_app_id(Some("evil.App"), false), "evil.App");
        assert_eq!(
            resolve_app_id(Some("org.eclipseos.Apotheosis.Scratch"), true),
            "org.eclipseos.Apotheosis.Scratch"
        );
        assert_eq!(
            resolve_app_id(Some("org.eclipseos.ApotheosisEvil"), true),
            d
        );
    }

    /// A keymap that moves Super to Mod3 (Hyper on Mod4): a fixed Mod4 index would miss it.
    const MOVED_SUPER: &str = r#"
xkb_keymap {
  xkb_keycodes "t" {
    minimum = 8; maximum = 255;
    <ESC> = 9; <LCTL> = 37; <LFSH> = 50; <LALT> = 64; <LWIN> = 133; <HYPR> = 207;
  };
  xkb_types "t" {
    type "ONE_LEVEL" { modifiers = None; map[None] = Level1; };
  };
  xkb_compat "t" {
    virtual_modifiers Alt, Super, Hyper;
    interpret.useModMapMods = AnyLevel;
    interpret Any + AnyOf(all) { action = SetMods(modifiers = modMapMods, clearLocks); };
    interpret Alt_L + AnyOf(all) { virtualModifier = Alt; action = SetMods(modifiers = modMapMods); };
    interpret Super_L + AnyOf(all) { virtualModifier = Super; action = SetMods(modifiers = modMapMods); };
    interpret Hyper_L + AnyOf(all) { virtualModifier = Hyper; action = SetMods(modifiers = modMapMods); };
  };
  xkb_symbols "t" {
    key <ESC> { type = "ONE_LEVEL", symbols[Group1] = [ Escape ] };
    key <LCTL> { [ Control_L ] };
    key <LFSH> { [ Shift_L ] };
    key <LALT> { [ Alt_L ] };
    key <LWIN> { [ Super_L ] };
    key <HYPR> { [ Hyper_L ] };
    modifier_map Shift { <LFSH> };
    modifier_map Control { <LCTL> };
    modifier_map Mod1 { <LALT> };
    modifier_map Mod3 { <LWIN> };
    modifier_map Mod4 { <HYPR> };
  };
};
"#;

    fn held_under(kb: &mut KbvmBackend, depressed: u32) -> Mods {
        kb.update_modifiers(Components {
            depressed,
            latched: 0,
            locked: 0,
            group: 0,
        });
        held_of(kb)
    }

    #[test]
    fn held_mods_resolve_by_name_not_bit_index() {
        let mut kb = KbvmBackend::new();
        kb.load_keymap(MOVED_SUPER).expect("keymap compiles");
        assert_eq!(held_under(&mut kb, 0), Mods::NONE);
        assert_eq!(held_under(&mut kb, 1 | 4), Mods::SHIFT | Mods::CTRL);
        assert_eq!(held_under(&mut kb, 8), Mods::ALT);
        // Super sits on Mod3 (0x20) here; Mod4 (0x40) is Hyper, not Super.
        assert_eq!(held_under(&mut kb, 0x20), Mods::SUPER);
        assert_eq!(held_under(&mut kb, 0x40), Mods::HYPER);
        // Lock bits (Caps = 2) are not held modifiers.
        assert_eq!(held_under(&mut kb, 2), Mods::NONE);
    }

    #[test]
    fn ime_commit_is_dropped_while_a_paste_confirm_is_pending() {
        let m = apoth_input::InputModes::default();
        assert_eq!(commit_bytes(false, "y", m), b"y");
        assert_eq!(commit_bytes(false, "\u{4e2d}", m), "\u{4e2d}".as_bytes());
        assert!(commit_bytes(true, "y", m).is_empty());
        assert!(commit_bytes(true, "rm -rf x", m).is_empty());
    }

    #[test]
    fn paste_bar_answers_keys_after_the_guard_only() {
        let spec = apoth_ui::bar::BarSpec::paste_confirm(WidgetId(1), 8, 1, "title");
        let guard = apoth_ui::tokens::motion::CONSENT_GUARD_MS;
        let ch = |c| apoth_input::Key::Char(c);
        // Inside the guard nothing answers, so a stray key cannot confirm a paste.
        assert_eq!(paste_answer(&spec, 0, ch('p')), None);
        assert_eq!(paste_answer(&spec, guard - 1, ch('p')), None);
        assert_eq!(paste_answer(&spec, guard, ch('p')), Some(0));
        assert_eq!(paste_answer(&spec, guard, ch('c')), Some(1));
        let esc = apoth_input::Key::Named(apoth_input::Named::Escape);
        assert_eq!(paste_answer(&spec, guard, esc), Some(1));
        // Return, Space and other keys never answer.
        let enter = apoth_input::Key::Named(apoth_input::Named::Enter);
        assert_eq!(paste_answer(&spec, guard, enter), None);
        assert_eq!(paste_answer(&spec, guard, ch(' ')), None);
        assert_eq!(paste_answer(&spec, guard, ch('x')), None);
    }

    #[test]
    fn consent_and_open_bars_ignore_keys_inside_the_guard() {
        // T-18 §12: keys typed within 500 ms of a consent bar appearing do not answer it.
        let guard = apoth_ui::tokens::motion::CONSENT_GUARD_MS;
        assert_eq!(guard, 500);
        let ch = |c| apoth_input::Key::Char(c);
        let spec = apoth_ui::bar::BarSpec::consent(
            WidgetId(1),
            crate::security::Class::ClipboardRead.label(),
            "t",
        );
        for k in ['o', 'a', 'd'] {
            assert_eq!(paste_answer(&spec, 0, ch(k)), None);
            assert_eq!(paste_answer(&spec, guard - 1, ch(k)), None);
        }
        assert_eq!(paste_answer(&spec, guard, ch('o')), Some(0));
        assert_eq!(paste_answer(&spec, guard, ch('a')), Some(1));
        assert_eq!(paste_answer(&spec, guard, ch('d')), Some(2));
        let enter = apoth_input::Key::Named(apoth_input::Named::Enter);
        assert_eq!(paste_answer(&spec, guard, enter), None);
        let open = open_bar(WidgetId(2), "https://e.example/");
        assert_eq!(paste_answer(&open, guard - 1, ch('o')), None);
    }

    #[test]
    fn handled_ids_are_sorted_registry_ids() {
        let mut sorted = HANDLED.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, HANDLED);
        for id in HANDLED {
            assert!(
                apoth_action::REGISTRY.iter().any(|i| i.id == *id),
                "{id} is not a registry id"
            );
        }
        assert!(handles("tab.new") && !handles("window.new"));
    }

    #[test]
    fn exit_codes() {
        assert_eq!(exit_code_of(ExitStatus::Code(3)), 3);
        assert_eq!(exit_code_of(ExitStatus::Signal(9)), 137);
    }
}
