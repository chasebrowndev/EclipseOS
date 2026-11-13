// SPDX-License-Identifier: AGPL-3.0-only
//! The main thread (T-00 §4.2): the calloop loop owning the Wayland connection, key encoding
//! and every PTY write. It never takes the pane's term lock on the input path and never
//! blocks on GPU, disk or a PTY write. Nothing typed, pasted or printed is ever logged.

use std::ffi::OsString;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use apoth_font::config::Style;
use apoth_font::discovery::{FontIndex, default_cache_file, font_dirs};
use apoth_font::system::{EmbeddedFont, FontSystem};
use apoth_input::backend::{Components, KeyboardBackend};
use apoth_input::backend_kbvm::KbvmBackend;
use apoth_input::{KeyKind, encode_focus, encode_key};
use apoth_pty::{
    CloseTimings, Enqueue, EnvSpec, ExitStatus, Pty, PtySize, SpawnConfig, build_env, choose_term,
    terminfo_dirs,
};
use apoth_term::Term;
use apoth_wl::{
    ConfigureEvent, InputEvent, InputKind, KeymapFormat, WindowId, WindowSpec, WlConfig, WlEvent,
    WlHandle,
};
use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken};

use rustix::io::Errno;

use crate::hook::DumpHook;
use crate::pane::{
    IoArgs, IoThread, MainMsg, PaneEvents, PaneShared, RenderMsg, ResizeReq, WAKE_RESIZE,
    WAKE_STOP, now_ns, spawn_io,
};
use crate::reload::{DEBOUNCE, POLL_PERIOD, WatchMode, Watcher};
use crate::render::{RenderArgs, RenderSettings, RenderUpdate, TermDefaults, pad_px};
use crate::settings::{ErrorPos, Settings, Startup, error_positions, load_input, resolve_shell};
use crate::ui::{Overlay, bar_inset_px, bars_for};
use crate::workers::Pool;
use apoth_config::fs::RealFs;
use apoth_config::reload::{Applied, Outcome, Reloader, Subsystem};
use apoth_config::types::Reload;
use apoth_ui::WidgetId;
/// Resize coalescing (apoth-pty leaves it to the caller): 25 ms idle.
const RESIZE_IDLE: Duration = Duration::from_millis(25);
/// After the child exits, wait this long for the last output before quitting.
const EXIT_GRACE: Duration = Duration::from_millis(250);
const MAX_COLS: usize = 1000;
const MAX_ROWS: usize = 500;

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

/// Grid size for a buffer: whole cells inside the padding.
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

struct Pane {
    shared: Arc<PaneShared>,
    pty: Option<Pty>,
    io: Option<IoThread>,
    render: Option<JoinHandle<()>>,
    render_tx: mpsc::Sender<RenderMsg>,
    cell: (u32, u32),
    /// The size the term and PTY currently have (rows, cols).
    dims: (usize, usize),
    pending: Option<(usize, usize)>,
    /// A resize the io thread has been asked for and has not confirmed.
    inflight: Option<(usize, usize)>,
    resize_timer: Option<RegistrationToken>,
    pollout: Option<RegistrationToken>,
    grace: Option<RegistrationToken>,
    child_exited: bool,
    eof: bool,
    closing: bool,
}

pub(crate) struct App {
    settings: Settings,
    command: Option<Vec<String>>,
    handle: WlHandle,
    lh: LoopHandle<'static, App>,
    signal: LoopSignal,
    win: WindowId,
    to_main: calloop::channel::Sender<MainMsg>,
    kb: KbvmBackend,
    startup: Option<ConfigureEvent>,
    pane: Option<Pane>,
    exit_code: i32,
    app_id: String,
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
    /// The compositor's blur path for the window (decides the auto opacity).
    blur: Option<apoth_wl::BlurPath>,
    auto_family: AutoFamily,
    /// The last terminal-set title, to re-render the title template on reload.
    term_title: String,
    /// Bumped when binds, key tables or macros change; the key dispatcher reads
    /// `config.binds` and friends and rebuilds its table when this moves.
    binds_generation: u64,
}

/// Run to completion; the process exit code.
pub(crate) fn run(
    startup: Startup,
    command: Option<Vec<String>>,
    app_id: Option<String>,
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
    let mut app = App {
        settings,
        command,
        handle,
        lh,
        signal: event_loop.get_signal(),
        win,
        to_main: tx,
        kb: KbvmBackend::new(),
        startup: None,
        pane: None,
        exit_code: 0,
        app_id,
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
        blur: None,
        auto_family: None,
        term_title: String::new(),
        binds_generation: 0,
    };
    app.start_watch(&files);
    app.set_config_errors(errors);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.pane.is_none() {
        event_loop
            .dispatch(Some(Duration::from_millis(100)), &mut app)
            .map_err(|e| e.to_string())?;
        if let Some(c) = app.startup.take() {
            app.start_pane(&c)?;
        }
        if Instant::now() > deadline {
            return Err("the compositor never configured the window".into());
        }
    }
    event_loop
        .run(None, &mut app, |_| {})
        .map_err(|e| e.to_string())?;
    app.shutdown();
    Ok(app.exit_code)
}

fn exit_code_of(s: ExitStatus) -> i32 {
    match s {
        ExitStatus::Code(c) => c,
        ExitStatus::Signal(n) => 128 + n,
    }
}

impl App {
    fn scale(&self) -> f64 {
        self.handle.scale(self.win).map_or(1.0, |s| s.as_f64())
    }

    /// Padding plus, at the top, the room the overlay bar takes (the grid never sits under it).
    fn pads(&self, scale: f64) -> (u32, u32) {
        let inset = self.bar.as_ref().map_or(0, |_| {
            bar_inset_px(scale, self.settings.glass.interface_zoom)
        });
        (
            pad_px(self.settings.padding.0, scale),
            pad_px(self.settings.padding.1, scale) + inset,
        )
    }

    fn start_pane(&mut self, c: &ConfigureEvent) -> Result<(), String> {
        self.blur = self.handle.blur_path(self.win);
        let scale = self.scale();
        let (fonts, metrics, auto_family) = build_fonts(&self.settings, scale)?;
        self.auto_family = auto_family;
        let cell = (metrics.width as u32, metrics.height as u32);
        let pad = self.pads(scale);
        if c.size.is_none() {
            let (irows, icols) = self.settings.initial_size;
            let lw = (f64::from(icols as u32 * cell.0 + 2 * pad.0) / scale).ceil() as u32;
            let lh = (f64::from(irows as u32 * cell.1 + 2 * pad.1) / scale).ceil() as u32;
            self.handle.set_window_size(self.win, (lw, lh));
        }
        let (_, buffer) = self
            .handle
            .surface_size(self.win)
            .ok_or("window has no size")?;
        let (rows, cols) = grid_for(buffer, pad, cell);

        let mut tc = self.settings.term.clone();
        tc.rows = rows;
        tc.cols = cols;
        tc.cell_px = cell;
        let term = Term::new(tc);
        let term_defaults = TermDefaults {
            fg: term.colors().0,
            bg: term.colors().1,
            cursor: term.colors().2,
            ansi: std::array::from_fn(|i| term.palette(i as u8)),
        };
        let shared = Arc::new(PaneShared::new(term));

        let size = PtySize::from_cells(rows as u16, cols as u16, cell.0, cell.1);
        let (argv, login) = match &self.command {
            Some(c) => (c.iter().map(OsString::from).collect(), false),
            None => {
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
            ("APOTHEOSIS_PANE".into(), "1".into()),
            ("APOTHEOSIS_WINDOW".into(), "1".into()),
        ];
        extra.extend(
            self.settings
                .shell
                .env
                .iter()
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
            cwd: self.settings.shell.cwd.clone(),
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
        let (render_tx, render_rx) = mpsc::channel();

        let io = spawn_io(
            IoArgs {
                shared: Arc::clone(&shared),
                master,
                wake_rx,
                to_main: self.to_main.clone(),
                to_render: render_tx.clone(),
                hook: DumpHook::from_env(),
            },
            wake_tx,
        )
        .map_err(|e| e.to_string())?;

        let surface = self
            .handle
            .surface_handle(self.win)
            .ok_or("window has no surface")?;
        let pacer = self.handle.pacer(self.win).ok_or("window has no pacer")?;
        let render = crate::render::spawn(RenderArgs {
            surface,
            pacer,
            shared: Arc::clone(&shared),
            fonts,
            settings: RenderSettings::from_settings(&self.settings, self.blur),
            term_defaults,
            scale,
            buffer,
            rx: render_rx,
            poke: render_tx.clone(),
            to_main: self.to_main.clone(),
        })
        .map_err(|e| e.to_string())?;

        self.lh
            .insert_source(
                Generic::new(pidfd, Interest::READ, Mode::Level),
                |_, _, app: &mut App| {
                    app.on_child_exit();
                    Ok(PostAction::Remove)
                },
            )
            .map_err(|e| e.error.to_string())?;
        tracing::info!(
            rows,
            cols,
            scale,
            app_id = %self.app_id,
            workers = self.workers.threads(),
            watch = ?self.watch_mode,
            "pane started"
        );
        self.pane = Some(Pane {
            shared,
            pty: Some(pty),
            io: Some(io),
            render: Some(render),
            render_tx,
            cell,
            dims: (rows, cols),
            pending: None,
            inflight: None,
            resize_timer: None,
            pollout: None,
            grace: None,
            child_exited: false,
            eof: false,
            closing: false,
        });
        // A bar raised by the startup load (or before the pane existed) reaches the renderer now.
        self.send_overlay();
        Ok(())
    }

    // ------------------------------------------------------------------ wayland

    fn on_wl(&mut self, ev: WlEvent) {
        match ev {
            WlEvent::Configure(c) => {
                if self.pane.is_some() {
                    self.recompute_dims();
                    self.poke();
                } else {
                    self.startup = Some(c);
                }
            }
            WlEvent::ScaleChanged { .. } => {
                self.recompute_dims();
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
            WlEvent::Fatal(f) => {
                tracing::error!(interface = %f.interface, code = f.code, "wayland protocol error");
                self.exit_code = 1;
                self.quit();
            }
            _ => {}
        }
    }

    fn poke(&self) {
        if let Some(p) = &self.pane {
            let _ = p.render_tx.send(RenderMsg::Poke);
        }
    }

    fn quit(&self) {
        self.signal.stop();
        self.signal.wakeup();
    }

    fn on_input(&mut self, i: InputEvent) {
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
            } => self.kb.update_modifiers(Components {
                depressed,
                latched,
                locked,
                group,
            }),
            InputKind::ModifiersReset => self.kb.update_modifiers(Components::default()),
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
        let Some(p) = &self.pane else { return };
        p.shared.focused.store(focused, Ordering::Release);
        let mut buf = [0u8; 3];
        let n = encode_focus(focused, p.shared.mirror.load(), &mut buf);
        if n > 0 {
            self.send_bytes(&buf[..n]);
        }
        self.poke();
    }

    /// Key to PTY: lookup, encode from the lock-free mirror, enqueue. Never touches the term.
    fn on_key(&mut self, key: u32, pressed: bool, repeat: bool) {
        if self.pane.is_none() {
            return;
        }
        let Some(look) = self.kb.lookup(key) else {
            return;
        };
        if pressed
            && !repeat
            && look.key == apoth_input::Key::Named(apoth_input::Named::Escape)
            && self.try_dismiss_bar()
        {
            return;
        }
        let Some(p) = &self.pane else { return };
        let kind = if !pressed {
            KeyKind::Release
        } else if repeat {
            KeyKind::Repeat
        } else {
            KeyKind::Press
        };
        let ev = look.event(kind);
        let mut buf = [0u8; 64];
        let n = encode_key(&ev, p.shared.mirror.load(), &mut buf);
        if n == 0 {
            return;
        }
        if pressed {
            p.shared.last_key_ns.store(now_ns(), Ordering::Release);
        }
        self.send_bytes(&buf[..n]);
    }

    /// Key and focus bytes: written straight to the master when nothing is queued (no
    /// allocation on the key to PTY path). Only a short write or a busy queue falls back to
    /// the owned-unit queue of `apoth-pty`, whose `send` takes a `Vec` (an API gap outside
    /// this crate; the slow path allocates, the common path does not).
    fn send_bytes(&mut self, bytes: &[u8]) {
        let Some(p) = self.pane.as_mut() else { return };
        let Some(pty) = p.pty.as_mut() else { return };
        if !pty.wants_pollout() {
            match rustix::io::write(pty.master_fd(), bytes) {
                Ok(n) if n == bytes.len() => return,
                Ok(n) => return self.send_unit(bytes[n..].to_vec()),
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(e) => {
                    tracing::warn!(errno = e.raw_os_error(), "pty write failed");
                    return;
                }
            }
        }
        self.send_unit(bytes.to_vec());
    }

    // ---------------------------------------------------------------- pty write

    fn send_unit(&mut self, unit: Vec<u8>) {
        let Some(p) = self.pane.as_mut() else { return };
        let Some(pty) = p.pty.as_mut() else { return };
        match pty.send(unit) {
            Ok(Enqueue::Written) => {}
            Ok(Enqueue::Queued) => self.arm_pollout(),
            Ok(Enqueue::Dropped) => {
                tracing::warn!(
                    dropped = pty.dropped_units(),
                    "outbound queue full; unit dropped"
                );
            }
            Err(e) => tracing::warn!(kind = ?e.kind(), "pty write failed"),
        }
    }

    fn arm_pollout(&mut self) {
        let Some(p) = self.pane.as_mut() else { return };
        if p.pollout.is_some() {
            return;
        }
        let Some(pty) = p.pty.as_ref() else { return };
        let Ok(fd) = pty.master_fd().try_clone_to_owned() else {
            return;
        };
        match self.lh.insert_source(
            Generic::new(fd, Interest::WRITE, Mode::Level),
            |_, _, app: &mut App| Ok(app.on_pollout()),
        ) {
            Ok(t) => p.pollout = Some(t),
            Err(e) => tracing::warn!(error = %e.error, "cannot watch the pty for writes"),
        }
    }

    fn on_pollout(&mut self) -> PostAction {
        let Some(p) = self.pane.as_mut() else {
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
            MainMsg::Reply(u) => self.send_unit(u),
            MainMsg::Resized(d) => self.on_resized(d),
            MainMsg::Events(e) => self.on_events(e),
            MainMsg::Eof => {
                if let Some(p) = self.pane.as_mut() {
                    p.eof = true;
                }
                self.maybe_finish();
            }
            MainMsg::RenderFailed(why) => {
                tracing::error!(%why, "rendering is unavailable");
                eprintln!("apotheosis: cannot render: {why}");
                self.exit_code = 69;
                self.begin_close();
            }
            MainMsg::CellSize(c) => {
                if let Some(p) = self.pane.as_mut() {
                    p.cell = c;
                }
                self.recompute_dims();
            }
            MainMsg::Loaded(r) => self.on_loaded(*r),
            MainMsg::Closed(status) => {
                if self.exit_code == 0 {
                    self.exit_code = status.map_or(0, exit_code_of).min(255);
                    // A close we asked for is a clean exit whatever signal ended the child.
                    if matches!(status, Some(ExitStatus::Signal(_))) {
                        self.exit_code = 0;
                    }
                }
                self.quit();
            }
        }
    }

    fn on_events(&mut self, e: PaneEvents) {
        if let Some(t) = e.title {
            self.handle
                .set_title(self.win, &self.settings.window_title(&t));
            self.term_title = t;
        }
        if e.bell > 0 && self.settings.bell_audible {
            self.handle.ring_bell(Some(self.win));
        }
    }

    // --------------------------------------------------------------- lifecycle

    fn on_child_exit(&mut self) {
        let Some(p) = self.pane.as_mut() else { return };
        let Some(pty) = p.pty.as_mut() else { return };
        match pty.try_wait() {
            Ok(Some(st)) => {
                self.exit_code = exit_code_of(st).min(255);
                p.child_exited = true;
                tracing::info!(code = self.exit_code, "child exited");
            }
            _ => return,
        }
        if p.grace.is_none() {
            match self
                .lh
                .insert_source(Timer::from_duration(EXIT_GRACE), |_, (), app: &mut App| {
                    if let Some(p) = app.pane.as_mut() {
                        p.grace = None;
                        p.eof = true;
                    }
                    app.maybe_finish();
                    TimeoutAction::Drop
                }) {
                Ok(t) => p.grace = Some(t),
                Err(_) => p.eof = true,
            }
        }
        self.maybe_finish();
    }

    fn maybe_finish(&mut self) {
        if let Some(p) = &self.pane
            && p.child_exited
            && p.eof
        {
            self.quit();
        }
    }

    /// Close escalation (T-18 §7) runs off the loop: SIGHUP, SIGTERM, SIGKILL with bounded waits.
    fn begin_close(&mut self) {
        let Some(p) = self.pane.as_mut() else {
            self.quit();
            return;
        };
        if p.closing {
            return;
        }
        p.closing = true;
        let Some(mut pty) = p.pty.take() else {
            self.quit();
            return;
        };
        let tx = self.to_main.clone();
        let spawned = std::thread::Builder::new()
            .name("apoth-close".into())
            .spawn(move || {
                let st = pty.close(&CloseTimings::default()).ok().flatten();
                let _ = tx.send(MainMsg::Closed(st));
            });
        if spawned.is_err() {
            self.quit();
        }
    }

    fn shutdown(&mut self) {
        if let Some(p) = self.pane.as_mut() {
            if let Some(io) = p.io.take() {
                let _ = rustix::io::write(&io.wake, &[WAKE_STOP]);
                let _ = io.join.join();
            }
            let _ = p.render_tx.send(RenderMsg::Stop);
            if let Some(r) = p.render.take() {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !r.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if r.is_finished() {
                    let _ = r.join();
                }
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
        self.set_config_errors(errors);
        if let Outcome::Applied(a) = outcome {
            let mut settings = Settings::from_config(self.reloader.config());
            settings.diagnostics = self.reloader.diagnostics().len();
            tracing::info!(
                keys = a.diff.keys.len(),
                collections = a.diff.collections.len(),
                "configuration reloaded"
            );
            self.apply_applied(settings, &a);
        }
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
        if f.render {
            self.send_render_update(f.font);
        }
        if f.title {
            self.handle
                .set_title(self.win, &self.settings.window_title(&self.term_title));
        }
        if self.settings.padding != old_padding {
            self.recompute_dims();
        }
        if f.keys {
            self.binds_generation += 1;
            tracing::info!(generation = self.binds_generation, "key bindings changed");
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

    /// Settings the render thread reads, with the font when `font.*` changed.
    fn send_render_update(&mut self, font: bool) {
        let settings = RenderSettings::from_settings(&self.settings, self.blur);
        let font = font.then(|| self.font_config());
        if let Some(p) = &self.pane {
            let _ = p
                .render_tx
                .send(RenderMsg::Update(Box::new(RenderUpdate { settings, font })));
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

    /// The one window's widget id; the binary has a single window until the mux lands.
    fn windows(&self) -> [WidgetId; 1] {
        [WidgetId(1)]
    }

    /// Rebuild the overlay from the current errors and restart keys, tell the render thread,
    /// and make room for it (or give the room back).
    fn refresh_bar(&mut self) {
        let spec = if self.bar_dismissed {
            None
        } else {
            bars_for(&self.windows(), &self.config_errors, &self.restart_keys)
                .into_iter()
                .find_map(|(_, b)| b)
        };
        let had = self.bar.is_some();
        self.bar = spec.map(|spec| Overlay {
            spec,
            shown_at: Instant::now(),
        });
        self.send_overlay();
        if had != self.bar.is_some() {
            self.recompute_dims();
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
        if let Some(p) = &self.pane {
            let _ = p
                .render_tx
                .send(RenderMsg::Overlay(self.bar.clone().map(Box::new)));
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

    fn recompute_dims(&mut self) {
        let scale = self.scale();
        let pad = self.pads(scale);
        let Some((_, buffer)) = self.handle.surface_size(self.win) else {
            return;
        };
        let Some(p) = self.pane.as_mut() else { return };
        let want = grid_for(buffer, pad, p.cell);
        if want == p.dims && p.pending.is_none() {
            return;
        }
        if p.pending == Some(want) || (p.pending.is_none() && p.inflight == Some(want)) {
            return;
        }
        p.pending = Some(want);
        if let Some(t) = p.resize_timer.take() {
            self.lh.remove(t);
        }
        match self
            .lh
            .insert_source(Timer::from_duration(RESIZE_IDLE), |_, (), app: &mut App| {
                app.apply_resize();
                TimeoutAction::Drop
            }) {
            Ok(t) => p.resize_timer = Some(t),
            Err(_) => self.apply_resize(),
        }
    }

    /// Hand the resize to the io thread, which applies `Term::resize` (a reflow can be long)
    /// under its own lock hold; main never takes the term lock. The size comes back as
    /// `MainMsg::Resized`, then `TIOCSWINSZ` follows.
    fn apply_resize(&mut self) {
        let Some(p) = self.pane.as_mut() else { return };
        p.resize_timer = None;
        let Some((rows, cols)) = p.pending.take() else {
            return;
        };
        let Some(io) = p.io.as_ref() else { return };
        p.shared.request_resize(ResizeReq {
            rows,
            cols,
            cell: p.cell,
        });
        p.inflight = Some((rows, cols));
        let _ = rustix::io::write(&io.wake, &[WAKE_RESIZE]);
    }

    fn on_resized(&mut self, (nr, nc): (usize, usize)) {
        let Some(p) = self.pane.as_mut() else { return };
        p.inflight = None;
        p.dims = (nr, nc);
        if let Some(pty) = p.pty.as_mut() {
            let size = PtySize::from_cells(nr as u16, nc as u16, p.cell.0, p.cell.1);
            if let Err(e) = pty.resize(size) {
                tracing::warn!(kind = ?e.kind(), "TIOCSWINSZ failed");
            }
        }
        tracing::debug!(rows = nr, cols = nc, "resized");
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

    #[test]
    fn exit_codes() {
        assert_eq!(exit_code_of(ExitStatus::Code(3)), 3);
        assert_eq!(exit_code_of(ExitStatus::Signal(9)), 137);
    }
}
