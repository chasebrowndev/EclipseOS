// SPDX-License-Identifier: AGPL-3.0-only
//! State, messages and view.
//!
//! The whole pane body is generated: `rows` is whatever the compositor said
//! its schema is, `place_for` sorts it onto a section's page, and `Control`
//! picks the widget. The one bespoke page is Display, which is not schema
//! keys at all but outputs (COMP-03 §1.1) — and even there the calibration overlay is the
//! compositor's; this pane only sends the verbs and shows the numbers.
//! Network is the other: the status service's readings, not config
//! (`network.rs`). Taskbar is schema keys arranged around a live picture of
//! the bar (`taskbar.rs`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::widget::{column, pick_list, row, scrollable, text_input, Column, Space};
use iced::{Element, Length, Subscription, Task, Theme};
use serde_json::{json, Value};

use ec_ipc::EventKind;
use ec_ui::motion::{Animated, Motion};
use ec_ui::theme;
use ec_ui::tokens::{color, font, radius, size, space};
use ec_ui::widget::{
    color_picker, content_at, dimmed_at, edge_note, hairline, header, list_row, list_row_at, micro_label,
    nav_item_at, nav_page_at, nav_section_at, panel, pill, pill_group, row_caption, sidebar_at, status_chip,
    subtitle, swatch_button, value as mono, Density, NumericSlider, Toggle,
};

mod display;

use crate::conn::{Conn, Problem};
use crate::network::{self, Net};
use crate::output::{Edge, Inset, Output};
use crate::pane::{page_of, place_for, Page, Section};
use crate::schema::{Control, Row as Key};
use crate::tray::{Tray, Writes};

const TRAY_PINNED: &str = "bar.tray.pinned";
const TRAY_HIDDEN: &str = "bar.tray.hidden";

/// How often the event thread checks the socket. `poll_event` never blocks and
/// we have no futures timer backend (only `thread-pool` is enabled), so the
/// wait is an ordinary sleep on an ordinary thread.
const POLL: Duration = Duration::from_millis(250);

/// The least time between two writes of one held slider: live enough that
/// the gaps move under the finger, sparse enough that a drag does not splice
/// the KDL file once per pixel.
const LIVE_WRITE_EVERY: Duration = Duration::from_millis(100);

/// The last write a held slider made.
struct LiveWrite {
    at: std::time::Instant,
    sent: Value,
    /// It was refused. The banner says so once; the drag stops writing until
    /// release, which tries again and reports again.
    failed: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// A section's head in the sidebar: fold it if it is the open one,
    /// otherwise open it on its first page.
    Section(Section),
    /// Show a page, its section expanded, scrolled to the top.
    Open(Page),
    /// Show a page and bring one of its rows into view, lit briefly — the
    /// door search (and anything else that names a setting) comes in by.
    /// Build one with [`locate`].
    Reveal(Locus),
    /// The sidebar's or a revealed row's frame clock, while either moves.
    NavFrame(Instant),
    Toggled(String, bool),
    SliderMoved(String, f64),
    SliderReleased(String),
    /// Give every unset mirror key on the pane (`Key::mirror_of`) a value
    /// of its own — the one it currently mirrors — so its row shows.
    SetApart,
    Chose(String, String),
    Edited(String, String),
    Committed(String),
    /// Open the file chooser portal for this key.
    /// Open or close the colour picker under this key's row.
    Picker(String),
    Browse(String),
    /// The portal answered: a path, a cancel (`None`) or why it could not.
    Browsed(String, Result<Option<std::path::PathBuf>, String>),
    Reload,
    Dismiss,
    /// "Clear usage history": forget what the launcher and the settings
    /// search remember about what was opened.
    ClearUsage,
    OutputEnabled(u64, bool),
    OutputScale(u64, f64),
    OutputScaleReleased(u64),
    /// A scale preset: written at once, no drag to wait out.
    OutputScaleSet(u64, f64),
    /// A mode in `set_output`'s own spelling (`WxH@mHz`).
    OutputMode(u64, String),
    OutputSelected(u64),
    /// An output dropped on the canvas, at its snapped logical position.
    OutputMoved(u64, i64, i64),
    OutputTransform(u64, String),
    InsetMoved(u64, Edge, f64),
    InsetReleased(u64),
    Calibrate(u64, &'static str),
    NumberTyped(Num, String),
    NumberCommitted(Num),
    /// A click landed somewhere else: every open draft commits or reverts.
    /// iced 0.14's `text_input` has no blur hook, so focus loss is observed
    /// at the application level instead.
    NumberBlur,
    Wire(EventKind, Value),
    /// The Network pane's feed connected; its action handle.
    NetReady(network::Handle),
    NetDown(String),
    Net(ec_services::status::Event),
    ForgetWifi(String),
    ForgetDevice(String),
    /// Select a tray entry, or clear the selection if it is the one selected.
    TraySelect(String),
    /// The live tray ids from `tray::feed`; `None` when it could not start.
    TrayLive(Option<Vec<String>>),
    /// The Taskbar pane's own messages.
    Bar(crate::taskbar::Msg),
    /// An edit to `bar.pinned-apps`.
    Pinned(crate::pinned::Msg),
    /// The sidebar search's messages.
    Search(crate::search_ui::Msg),
    /// Launch the policy viewer, from the Oracle Eyes capture hero.
    OpenPolicyViewer,
    /// The Animations page's own messages.
    Anim(crate::animations::Msg),
    /// The Accounts page's own messages.
    Accounts(crate::accounts::Msg),
}

/// Somewhere to take the user: a page, and optionally a row on it by its
/// [`row_id`] (a config key's dotted path). See [`locate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locus {
    pub page: Page,
    pub row: Option<String>,
}

/// Where `target` is, or `None` if nothing here shows it. `target` is any of:
///
/// - a config key (`decoration.blur.size`): its page, and its row;
/// - a page path (`desktop/wallpaper`, `effects`, `addons`), as
///   [`Page::from_arg`] reads it: that page, no row;
/// - a bespoke-pane place as search names it (`pane:display.scale`,
///   `pane:taskbar.widgets`): the page the part after `pane:` names, read
///   as `section.page`, no row.
pub fn locate(target: &str) -> Option<Locus> {
    if let Some(place) = target.strip_prefix(BESPOKE) {
        let path = place.replacen('.', "/", 1);
        return Page::from_arg(&path).map(|page| Locus { page, row: None });
    }
    if let Some(place) = place_for(target) {
        let row = if place.page == Page::Animations {
            crate::animations::row_of(target)
        } else {
            target.to_owned()
        };
        return Some(Locus {
            page: place.page,
            row: Some(row),
        });
    }
    Page::from_arg(target).map(|page| Locus { page, row: None })
}

/// The terminal, and the launcher switch it gates (`schema_page`).
const TERMINAL: &str = "misc.terminal-command";
const TERMINAL_APPS: &str = "launcher.search.terminal-apps";

/// How search names a place in a bespoke pane (`search::BESPOKE`).
const BESPOKE: &str = "pane:";

/// How long a revealed row stays lit.
const REVEAL: Duration = Duration::from_millis(ec_ui::tokens::motion::REVEAL_MS);

/// The widget id of the row that shows config key `path`: what
/// [`Message::Reveal`] scrolls to and lights.
pub fn row_id(path: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("row:{path}"))
}

/// Which draggable number a typed draft belongs to. The three sites are not
/// one keyspace — a schema key is a path, an output control is an id — so the
/// draft map is keyed by the union rather than by a stringly-typed hybrid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Num {
    Key(String),
    Inset(u64, Edge),
    Scale(u64),
}

/// The value a [`Num`] is allowed to take: its slider's own range, and
/// whether it reads as an integer.
struct Span {
    min: f64,
    max: f64,
    integral: bool,
}

/// Overscan is bounded here and nowhere else — the compositor's own clamp is
/// a fraction of the mode (`ec-abyss-render/src/output_overscan.rs`), so this is a UI bound.
const INSET_MAX: f64 = 120.0;
const SCALE_MIN: f64 = 0.5;
const SCALE_MAX: f64 = 3.0;
const SCALE_STEP: f64 = 0.05;

impl Span {
    fn format(&self, v: f64) -> String {
        if self.integral {
            format!("{}", v.round() as i64)
        } else {
            format!("{v:.2}")
        }
    }

    /// Out of range is clamped, like a drag to the end of the track.
    /// Unparseable is refused, and `None` is what refusal looks like.
    fn parse(&self, text: &str) -> Option<f64> {
        let v: f64 = text.trim().parse().ok()?;
        v.is_finite().then(|| v.clamp(self.min, self.max))
    }
}

pub struct App {
    pub(crate) conn: Conn,
    pub(crate) rows: Vec<Key>,
    /// The page the content column shows.
    page: Page,
    /// The expanded section in the sidebar: at most one, and not always the
    /// shown page's — folding the current section leaves its page showing.
    open: Option<Section>,
    /// Each expanding section's open fraction, 0 folded to 1 open, moving
    /// toward `open` under the default motion.
    folds: HashMap<Section, Animated>,
    /// The row a [`Message::Reveal`] lit, and when: it fades over
    /// `motion::REVEAL_MS`.
    flash: Option<(String, Instant)>,
    /// Text in flight, per path. Absent means "show the committed value".
    drafts: HashMap<String, String>,
    /// Drafts `validate_config` has rejected, with the compositor's reason.
    /// Written on every keystroke so the field can say no, and why, before
    /// the user commits.
    invalid: HashMap<String, String>,
    /// The colour key whose picker is open under its row.
    picker: Option<String>,
    /// Text typed into a numeric entry, per control. Absent means "show the
    /// value the slider is at".
    nums: HashMap<Num, String>,
    /// Slider position while the knob is held. It is what the slider shows
    /// until release, so the compositor echoing a live write back (a
    /// `Config` event, a `refresh`) can never pull the knob from the finger.
    pub(crate) live: HashMap<String, f64>,
    /// The throttled writes of the sliders being held, per path.
    live_writes: HashMap<String, LiveWrite>,
    outputs: Vec<Output>,
    /// The output the Display card is about; `None` is the focused one.
    selected: Option<u64>,
    insets: HashMap<u64, Inset>,
    scales: HashMap<u64, f64>,
    calibrating: Option<u64>,
    pub(crate) banner: Option<Problem>,
    restart_pending: bool,
    net: Net,
    /// The tray entry the move pills act on.
    pub(crate) tray_sel: Option<String>,
    /// The apps `bar.pinned-apps` can take, scanned once at start.
    pub(crate) installed: Vec<crate::pinned::Choice>,
    /// The running status-notifier items, while the Taskbar pane shows.
    /// `None` is not heard from yet; `Some(None)` is the feed failing.
    pub(crate) tray_live: Option<Option<Vec<String>>>,
    /// Every tray id the pane has listed at a move, kept for the session
    /// (TRAY-01, [`Tray::seen`]).
    pub(crate) tray_seen: Vec<String>,
    /// Every panel's glass radius, live-synced to `decoration.rounding`
    /// (BLUR-06): read once at startup and refetched on every `Config` event,
    /// so a live-reload can never leave this pane's glass drifted from the
    /// compositor's blur backdrop behind it.
    pub(crate) glass_radius: f32,
    /// Whether the compositor's blur is behind the window
    /// (`decoration.blur.mode != "off"`), synced like `glass_radius`. The
    /// window is transparent, so this decides whether the sidebar is
    /// translucent glass or its opaque backed fallback.
    pub(crate) blur: bool,
    /// The Taskbar pane's picture, lane and editor.
    pub(crate) bar: crate::taskbar::Bar,
    /// Installed add-ons and the hooks they turn on (ADR 0066). `None` until
    /// the compositor answers.
    pub(crate) addons: Option<ec_ipc::Addons>,
    /// The sidebar search: its index over `rows`, the query and its hits.
    pub(crate) search: crate::search_ui::Search,
    /// The "Clear usage history" button's last press, and whether it worked.
    /// Shown for `CLEARED_FOR`, then the button comes back.
    cleared: Option<(Instant, bool)>,
    /// Oracle Eyes' capture grant, read off `policy.kdl` with the widgets.
    pub(crate) oe_grant: crate::oracle::Grant,
    /// The policy viewer this app launched, kept so it is reaped and a
    /// second click while it runs does not open a second one.
    viewer: Option<std::process::Child>,
    /// The Animations page's looping previews.
    pub(crate) anim: crate::animations::Anim,
    /// The Accounts page: the store, its account names, and a sign-in.
    pub(crate) accounts: crate::accounts::Accounts,
}

/// How long "Cleared" stands in for the button.
const CLEARED_FOR: Duration = Duration::from_millis(2500);

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_page(Section::Windows.first())
    }

    /// Open on `page`, its section expanded — `ec-settings network`
    /// from the taskbar, `ec-settings desktop/wallpaper`.
    pub fn with_page(page: Page) -> Self {
        let mut conn = Conn::new();
        let glass_radius = conn.glass_radius().unwrap_or(ec_ui::tokens::radius::CARD);
        let blur = conn.blur().unwrap_or(false);
        let mut app = App {
            conn,
            rows: Vec::new(),
            page,
            open: Some(page.section()),
            folds: Section::ALL
                .iter()
                .filter(|s| s.expands())
                .map(|s| {
                    let at = if *s == page.section() { 1.0 } else { 0.0 };
                    (*s, Animated::new(at, Motion::DEFAULT))
                })
                .collect(),
            flash: None,
            drafts: HashMap::new(),
            invalid: HashMap::new(),
            picker: None,
            live: HashMap::new(),
            live_writes: HashMap::new(),
            nums: HashMap::new(),
            outputs: Vec::new(),
            selected: None,
            insets: HashMap::new(),
            scales: HashMap::new(),
            calibrating: None,
            banner: None,
            restart_pending: false,
            net: Net::default(),
            tray_sel: None,
            installed: crate::pinned::installed(),
            tray_live: None,
            tray_seen: Vec::new(),
            glass_radius,
            blur,
            bar: crate::taskbar::Bar::default(),
            addons: None,
            search: crate::search_ui::Search::default(),
            cleared: None,
            oe_grant: crate::oracle::Grant::Unread,
            viewer: None,
            anim: crate::animations::Anim::default(),
            accounts: crate::accounts::Accounts::default(),
        };
        // Debug builds only: open with a tray entry selected, so the selected
        // state can be screenshotted without pointer injection.
        #[cfg(debug_assertions)]
        {
            app.tray_sel = std::env::var("SETTINGS_PREVIEW_TRAY_SEL").ok();
            app.picker = std::env::var("SETTINGS_PREVIEW_PICKER").ok();
        }
        app.reload();
        #[cfg(debug_assertions)]
        crate::taskbar::preview_env(&mut app);
        #[cfg(debug_assertions)]
        crate::accounts::preview_env(&mut app.accounts);
        if app.page == Page::Animations {
            crate::animations::sync(&mut app);
        }
        crate::taskbar::adopt_selection(&mut app);
        app
    }

    /// Re-read everything. Cheaper than tracking which key a write touched,
    /// and it is also how the app recovers from someone editing the file.
    pub(crate) fn reload(&mut self) {
        match self.conn.load_schema() {
            Ok(rows) => {
                self.rows = rows;
                self.drafts.clear();
                self.invalid.clear();
                self.live.clear();
                self.nums.clear();
            }
            Err(e) => self.banner = Some(e),
        }
        // Keys this compositor did not report still get a control, so the
        // Taskbar pane is whole with no socket and ahead of a newer schema.
        crate::taskbar::stand_in(&mut self.rows);
        self.search.reindex(&self.rows);
        self.read_widgets();
        match self.conn.call("get_outputs", json!({ "all": true })) {
            Ok(reply) => {
                self.outputs = crate::output::parse_all(&reply);
                self.scales.clear();
            }
            Err(e) => self.banner = Some(e),
        }
    }

    /// Re-read the values after the config changed underneath, keeping
    /// what the user has typed and not yet committed. [`App::reload`] is
    /// for when the app itself changed something and wants a clean slate.
    pub(crate) fn refresh(&mut self) {
        if let Ok(rows) = self.conn.load_schema() {
            self.rows = rows;
            crate::taskbar::stand_in(&mut self.rows);
            self.search.reindex(&self.rows);
        }
        self.read_widgets();
    }

    /// The widget blocks, their approval states, and the add-ons that decide
    /// whether there are any: one read, because the three change together
    /// (an add-on installed, a widget withheld, a prompt answered).
    fn read_widgets(&mut self) {
        self.addons = self.conn.addons().ok();
        self.oe_grant = crate::oracle::read_grant();
        if let Ok(w) = self.conn.widgets() {
            self.bar.customs = w;
        }
        if let Ok(s) = self.conn.widget_statuses() {
            self.bar.statuses = s;
        }
    }

    /// A text field's draft as the JSON its key takes: a string, or for a
    /// string-list key (the model command's argv line) the words as a list.
    fn typed(&self, path: &str, text: &str) -> Value {
        match self.key(path).map(|k| &k.control) {
            Some(Control::List) => crate::schema::argv_value(text),
            _ => Value::String(text.to_owned()),
        }
    }

    pub(crate) fn key(&self, path: &str) -> Option<&Key> {
        self.rows.iter().find(|r| r.path == path)
    }

    /// Whether keyboard hint lines show (`ui.show-key-hints`). On when the
    /// compositor does not report the key: hints are the shipped default.
    pub(crate) fn key_hints_on(&self) -> bool {
        self.key("ui.show-key-hints").is_none_or(Key::as_bool)
    }

    /// The two tray lists as the compositor last reported them.
    pub(crate) fn tray(&self) -> Tray {
        let v = |p| self.key(p).map(|k| k.value.clone()).unwrap_or(Value::Null);
        let mut t = Tray::from_values(&v(TRAY_PINNED), &v(TRAY_HIDDEN));
        if let Some(Some(live)) = &self.tray_live {
            t.live = live.clone();
        }
        t.seen = self.tray_seen.clone();
        t
    }

    /// Write a tray move: only the lists it changed.
    pub(crate) fn write_tray(&mut self, w: Writes) {
        self.tray_seen = self.tray().ids();
        if let Some(p) = w.pinned {
            self.write(TRAY_PINNED, json!(p));
        }
        if let Some(h) = w.hidden {
            self.write(TRAY_HIDDEN, json!(h));
        }
    }

    /// Write one scalar and fold the outcome into the banner.
    pub(crate) fn write(&mut self, path: &str, v: Value) {
        match self.conn.set(path, v) {
            Ok(restart) => {
                self.restart_pending |= restart;
                self.banner = None;
                self.reload();
            }
            Err(e) => self.banner = Some(e),
        }
    }

    /// Write several scalars as one commit (`null` removes a key), and fold
    /// the outcome into the banner as [`App::write`] does.
    pub(crate) fn write_many(&mut self, edits: &[(String, Value)]) {
        match self.conn.set_many(edits) {
            Ok(restart) => {
                self.restart_pending |= restart;
                self.banner = None;
                self.reload();
            }
            Err(e) => self.banner = Some(e),
        }
    }

    /// A slider reading as the JSON its key takes: whole for an int key.
    fn slider_json(&self, path: &str, v: f64) -> Value {
        match self.key(path).map(|k| &k.control) {
            Some(Control::Slider { integral: true, .. }) => json!(v.round() as i64),
            _ => json!(v),
        }
    }

    /// What a key amounts to right now: its own value, or — unset and
    /// mirroring another (`Key::mirror_of`) — the value it mirrors.
    pub(crate) fn effective(&self, key: &Key) -> f64 {
        match key.mirror_of().filter(|_| key.value.is_null()) {
            Some(source) => self.key(source).map_or(0.0, Key::as_f64),
            None => key.as_f64(),
        }
    }

    /// A held slider's throttled write: at most one per `LIVE_WRITE_EVERY`,
    /// none when the value has not changed, and no `reload` — that would
    /// clear `live` and drop the knob. The release makes the final write.
    fn write_live(&mut self, path: &str, v: f64) {
        let now = std::time::Instant::now();
        let json = self.slider_json(path, v);
        if let Some(last) = self.live_writes.get(path) {
            if last.failed || last.sent == json || now.duration_since(last.at) < LIVE_WRITE_EVERY {
                return;
            }
        }
        let failed = match self.conn.set(path, json.clone()) {
            Ok(restart) => {
                self.restart_pending |= restart;
                false
            }
            Err(e) => {
                self.banner = Some(e);
                true
            }
        };
        self.live_writes.insert(
            path.to_owned(),
            LiveWrite {
                at: now,
                sent: json,
                failed,
            },
        );
    }

    /// The range a typed draft is checked against: the same one its slider
    /// spans, so typing and dragging cannot disagree.
    fn span(&self, id: &Num) -> Option<Span> {
        match id {
            Num::Key(path) => match self.key(path).map(|k| &k.control) {
                // The typed-input path always validates against the true
                // `max`, never the slider's narrower `drag_max` — a drag
                // stays fine-grained, but a typed number still reaches the
                // real limit.
                Some(Control::Slider {
                    min, max, integral, ..
                }) => Some(Span {
                    min: *min,
                    max: *max,
                    integral: *integral,
                }),
                _ => None,
            },
            Num::Inset(..) => Some(Span {
                min: 0.0,
                max: INSET_MAX,
                integral: true,
            }),
            Num::Scale(..) => Some(Span {
                min: SCALE_MIN,
                max: SCALE_MAX,
                integral: false,
            }),
        }
    }

    /// What the control reads right now: the in-flight drag if there is one,
    /// otherwise the compositor's value.
    fn current(&self, id: &Num) -> Option<f64> {
        match id {
            Num::Key(path) => self
                .live
                .get(path)
                .copied()
                .or_else(|| self.key(path).map(Key::as_f64)),
            Num::Inset(out, edge) => {
                let o = self.outputs.iter().find(|o| o.id == *out)?;
                Some(self.insets.get(out).copied().unwrap_or(o.overscan).get(*edge) as f64)
            }
            Num::Scale(out) => {
                let o = self.outputs.iter().find(|o| o.id == *out)?;
                Some(self.scales.get(out).copied().unwrap_or(o.scale))
            }
        }
    }

    /// Apply a committed draft. The write is the same one the slider's
    /// release performs — a typed number is not a second code path.
    fn commit_num(&mut self, id: &Num, value: f64) {
        match id {
            Num::Key(path) => {
                let integral = matches!(
                    self.key(path).map(|k| &k.control),
                    Some(Control::Slider { integral: true, .. })
                );
                self.live.remove(path);
                let json = if integral {
                    json!(value.round() as i64)
                } else {
                    json!(value)
                };
                let path = path.clone();
                self.write(&path, json);
            }
            Num::Inset(out, edge) => {
                let base = self
                    .outputs
                    .iter()
                    .find(|o| o.id == *out)
                    .map(|o| o.overscan)
                    .unwrap_or_default();
                let inset = self.insets.entry(*out).or_insert(base);
                inset.set(*edge, value.round() as i64);
                let inset = *inset;
                self.set_output(*out, "overscan", inset.to_json());
                self.insets.remove(out);
            }
            Num::Scale(out) => {
                self.scales.remove(out);
                self.set_output(*out, "scale", json!(value));
            }
        }
    }

    fn set_output(&mut self, id: u64, field: &str, v: Value) {
        // `output` is the numeric id from `get_outputs`, not the connector
        // name: every handler parses it with `u64_param`.
        if !self.outputs.iter().any(|o| o.id == id) {
            return;
        }
        match self.conn.call("set_output", json!({ "output": id, field: v })) {
            Ok(_) => {
                self.banner = None;
                self.reload();
            }
            Err(e) => self.banner = Some(e),
        }
    }
}

/// How long past boot a debug build's `SETTINGS_PREVIEW_REVEAL` row stays
/// fully lit before it starts to fade.
#[cfg(debug_assertions)]
const PREVIEW_HOLD: Duration = Duration::from_secs(10);

/// The content column's scrollable, so a debug build can open scrolled.
const SCROLL: &str = "content";

/// Start on `page`. Debug builds read `SETTINGS_PREVIEW_SCROLL` (logical px)
/// and open scrolled that far, so a lower block can be screenshotted with no
/// pointer to scroll it, and `SETTINGS_PREVIEW_REVEAL` (anything [`locate`]
/// reads) and open on that row, lit, as a search jump would.
/// `SETTINGS_PREVIEW_SEARCH` opens with that query in the sidebar search,
/// and `SETTINGS_PREVIEW_SEARCH_SEL` with that result selected.
pub fn boot(page: Page) -> (App, Task<Message>) {
    #[allow(unused_mut)]
    let (app, mut task) = boot_inner(page);
    #[cfg(debug_assertions)]
    if let Ok(path) = std::env::var("SETTINGS_PREVIEW_SHOT") {
        task = Task::batch([task, preview_shot(path)]);
    }
    (app, task)
}

/// How long a debug build's `SETTINGS_PREVIEW_SHOT` waits for the window to
/// map and settle before it reads the frame back.
#[cfg(debug_assertions)]
const SHOT_AFTER: Duration = Duration::from_millis(3000);

/// Debug builds only: read the window's own frame back after
/// [`SHOT_AFTER`], write it to `path` as `WxH\n` and raw RGBA, and exit. The
/// screenshot loop on a host with no screencopy client (a headless abyss
/// in CI) has no other way to see the pane.
#[cfg(debug_assertions)]
fn preview_shot(path: String) -> Task<Message> {
    let (tx, rx) = iced::futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(SHOT_AFTER);
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

fn boot_inner(page: Page) -> (App, Task<Message>) {
    #[allow(unused_mut)]
    let mut app = App::with_page(page);
    #[cfg(debug_assertions)]
    if let Ok(q) = std::env::var("SETTINGS_PREVIEW_SEARCH") {
        app.search.set_query(&q);
        if let Some(n) = std::env::var("SETTINGS_PREVIEW_SEARCH_SEL")
            .ok()
            .and_then(|n| n.parse().ok())
        {
            app.search.select(n);
        }
    }
    // `SETTINGS_PREVIEW_EDIT=path=text`: open with that draft typed into
    // the key's field, validated as a keystroke would be, so a refusal can be
    // screenshotted without injected typing.
    #[cfg(debug_assertions)]
    if let Some((path, text)) = std::env::var("SETTINGS_PREVIEW_EDIT")
        .ok()
        .and_then(|e| e.split_once('=').map(|(p, t)| (p.to_owned(), t.to_owned())))
    {
        let _ = update(&mut app, Message::Edited(path, text));
    }
    #[cfg(debug_assertions)]
    if let Some(to) = std::env::var("SETTINGS_PREVIEW_REVEAL")
        .ok()
        .and_then(|t| locate(&t))
    {
        let mut app = app;
        let task = update(&mut app, Message::Reveal(to));
        // The window takes seconds to map, longer than the light lasts:
        // start the fade later, so a screenshot can still catch it lit.
        if let Some((_, at)) = &mut app.flash {
            *at += PREVIEW_HOLD;
        }
        return (app, task);
    }
    #[cfg(debug_assertions)]
    if let Some(y) = std::env::var("SETTINGS_PREVIEW_SCROLL")
        .ok()
        .and_then(|y| y.parse::<f32>().ok())
    {
        let to = iced::widget::scrollable::AbsoluteOffset { x: None, y: Some(y) };
        return (app, iced::widget::operation::scroll_to(SCROLL, to));
    }
    (app, Task::none())
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    // The previews' frame clock moves nothing they are built from.
    let frame = matches!(message, Message::Anim(crate::animations::Msg::Frame(_)));
    let task = update_inner(app, message);
    if app.page == Page::Animations && !frame {
        crate::animations::sync(app);
    }
    // Any message can move the bar picture: a knob, the order, a reload.
    if app.page.section() == Section::Taskbar {
        crate::taskbar::sync(app, std::time::Instant::now());
    }
    task
}

fn update_inner(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Section(s) => {
            if app.open == Some(s) && s.expands() {
                // Folding leaves the page where it is: closing a list is
                // not a request to go somewhere else.
                app.fold(None);
            } else if app.page.section() == s {
                // Reopening the section the page is in: just unfold it.
                app.fold(Some(s));
            } else {
                return go(app, s.first());
            }
        }
        Message::Open(p) => return go(app, p),
        Message::Reveal(Locus { page, row }) => {
            let top = go(app, page);
            let Some(path) = row else { return top };
            app.flash = Some((path.clone(), Instant::now()));
            return iced::advanced::widget::operate(Measure::new(row_id(&path))).then(|at| match at {
                Some(y) => iced::widget::operation::scroll_to(
                    SCROLL,
                    iced::widget::scrollable::AbsoluteOffset {
                        x: None,
                        y: Some((y - space::BLOCK).max(0.0)),
                    },
                ),
                None => Task::none(),
            });
        }
        Message::NavFrame(now) => {
            for f in app.folds.values_mut() {
                f.tick(now);
            }
            if app
                .cleared
                .is_some_and(|(at, ok)| ok && now.duration_since(at) >= CLEARED_FOR)
            {
                app.cleared = None;
            }
            if app
                .flash
                .as_ref()
                .is_some_and(|(_, at)| now.duration_since(*at) >= REVEAL)
            {
                app.flash = None;
            }
        }
        Message::Dismiss => app.banner = None,
        Message::ClearUsage => {
            // Under test the real history is never touched.
            let done = cfg!(test) || ec_services::frecency::clear_all().is_ok();
            if done {
                app.search.forget();
            }
            app.cleared = Some((Instant::now(), done));
        }
        Message::OpenPolicyViewer => {
            let running = app
                .viewer
                .as_mut()
                .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
            if !running {
                match std::process::Command::new("ec-policy-viewer").spawn() {
                    Ok(child) => app.viewer = Some(child),
                    Err(e) => {
                        app.viewer = None;
                        app.banner = Some(Problem::Other(format!("could not start ec-policy-viewer: {e}")));
                    }
                }
            }
        }
        Message::Reload => app.reload(),

        Message::Toggled(path, on) => app.write(&path, Value::Bool(on)),

        Message::SliderMoved(path, v) => {
            // A key its schema says needs a restart gains nothing from a
            // live write but a pile of "restart to apply" churn.
            if app.key(&path).is_some_and(|k| !k.needs_restart && !k.locked()) {
                app.write_live(&path, v);
            }
            app.live.insert(path, v);
        }
        Message::SliderReleased(path) => {
            app.live_writes.remove(&path);
            if let Some(v) = app.live.remove(&path) {
                let json = app.slider_json(&path, v);
                app.write(&path, json);
            }
        }
        Message::SetApart => {
            let apart: Vec<(String, Value)> = app
                .rows
                .iter()
                .filter(|k| page_of(&k.path) == Some(app.page) && hidden_mirror(k))
                .map(|k| (k.path.clone(), app.slider_json(&k.path, app.effective(k))))
                .collect();
            for (path, json) in apart {
                app.write(&path, json);
            }
        }

        Message::Chose(path, choice) => app.write(&path, Value::String(choice)),

        Message::Edited(path, text) => {
            // Validation is per keystroke so the field can refuse before the
            // write. It is a `dry_run`-shaped call: nothing is spliced.
            match app.conn.validate(&path, app.typed(&path, &text)) {
                Ok(()) => {
                    app.invalid.remove(&path);
                }
                Err(e) => {
                    app.invalid.insert(path.clone(), e.detail().to_owned());
                }
            }
            app.drafts.insert(path, text);
        }
        Message::Committed(path) => {
            if app.invalid.contains_key(&path) {
                return Task::none();
            }
            if let Some(text) = app.drafts.remove(&path) {
                let v = app.typed(&path, &text);
                app.write(&path, v);
            }
        }

        Message::Picker(path) => {
            if app.picker.as_deref() == Some(path.as_str()) {
                app.picker = None;
                // Closing lands whatever the picker or the field left pending.
                return update_inner(app, Message::Committed(path));
            }
            app.picker = Some(path);
        }

        Message::Browse(path) => {
            return Task::perform(crate::portal::pick_image(), move |r| {
                Message::Browsed(path.clone(), r)
            });
        }
        Message::Browsed(path, result) => match result {
            Ok(Some(p)) => {
                let text = p.to_string_lossy().into_owned();
                let _ = update_inner(app, Message::Edited(path.clone(), text));
                return update_inner(app, Message::Committed(path));
            }
            Ok(None) => {}
            Err(e) => app.banner = Some(Problem::Other(e)),
        },

        Message::NumberTyped(id, text) => {
            app.nums.insert(id, text);
        }
        Message::NumberCommitted(id) => {
            if let Some(text) = app.nums.remove(&id) {
                if let Some(v) = app.span(&id).and_then(|s| s.parse(&text)) {
                    app.commit_num(&id, v);
                }
            }
        }
        Message::NumberBlur => {
            // Take first: a write reloads, and a reload clears the map out
            // from under an iteration over it.
            for (id, text) in std::mem::take(&mut app.nums) {
                let Some(span) = app.span(&id) else { continue };
                // Unchanged text is not a write — a click inside the field
                // must not splice the file.
                match (span.parse(&text), app.current(&id)) {
                    (Some(v), Some(now)) if (v - now).abs() > f64::EPSILON => app.commit_num(&id, v),
                    _ => {}
                }
            }
        }

        Message::OutputEnabled(id, on) => app.set_output(id, "enabled", Value::Bool(on)),
        Message::OutputScale(id, v) => {
            app.scales.insert(id, v);
        }
        Message::OutputScaleReleased(id) => {
            if let Some(v) = app.scales.remove(&id) {
                app.set_output(id, "scale", json!(v));
            }
        }
        Message::OutputScaleSet(id, v) => {
            app.scales.remove(&id);
            app.set_output(id, "scale", json!(v));
        }
        Message::OutputMode(id, m) => app.set_output(id, "mode", Value::String(m)),
        Message::OutputSelected(id) => app.pick_output(id),
        Message::OutputMoved(id, x, y) => app.move_output(id, x, y),
        Message::OutputTransform(id, t) => app.set_output(id, "transform", Value::String(t)),
        Message::InsetMoved(id, edge, v) => {
            let inset = app.insets.entry(id).or_default();
            inset.set(edge, v.round() as i64);
        }
        Message::InsetReleased(id) => {
            if let Some(inset) = app.insets.get(&id).copied() {
                app.set_output(id, "overscan", inset.to_json());
                // The compositor's reading is the truth from here on; the
                // draft would otherwise pin the row to a stale number.
                app.insets.remove(&id);
            }
        }
        Message::Calibrate(id, action) => {
            if !app.outputs.iter().any(|o| o.id == id) {
                return Task::none();
            }
            match app
                .conn
                .call("calibrate_output", json!({ "output": id, "action": action }))
            {
                Ok(_) => {
                    app.calibrating = (action == "start").then_some(id);
                    app.banner = None;
                    // Commit and cancel both leave the compositor holding
                    // overscan numbers this pane has never seen.
                    if action != "start" {
                        app.insets.remove(&id);
                        app.reload();
                    }
                }
                Err(e) => app.banner = Some(e),
            }
        }

        Message::Wire(kind, data) => match kind {
            // Hot-plug: the output list is the compositor's, never cached
            // across an event.
            EventKind::Output => app.reload(),
            EventKind::ConfigError => app.banner = Some(Problem::from_config_error(&data)),
            // A reload succeeded; re-read the one key this pane keeps live
            // between fetches rather than re-deriving from `rows` (BLUR-06).
            EventKind::Config => {
                if let Some(radius) = app.conn.glass_radius() {
                    app.glass_radius = radius;
                }
                if let Some(blur) = app.conn.blur() {
                    app.blur = blur;
                }
                app.refresh();
                crate::taskbar::follow_config(app);
            }
            _ => {}
        },

        Message::NetReady(h) => app.net.ready(h),
        Message::NetDown(why) => app.net.down(why),
        Message::Net(ev) => app.net.apply(ev),
        Message::ForgetWifi(ssid) => app.net.forget_wifi(ssid),
        Message::ForgetDevice(addr) => app.net.forget_device(addr),

        Message::TraySelect(id) => {
            app.tray_sel = (app.tray_sel.as_deref() != Some(id.as_str())).then_some(id);
        }
        Message::TrayLive(live) => app.tray_live = Some(live),
        Message::Bar(m) => return crate::taskbar::update(app, m),
        Message::Pinned(m) => crate::pinned::update(app, m),
        Message::Search(m) => return crate::search_ui::update(app, m),
        Message::Anim(m) => return crate::animations::update(app, m),
        Message::Accounts(m) => return crate::accounts::update(app, m),
    }
    Task::none()
}

/// Show `page`: its section expanded (and any other folded), the content
/// column back at the top.
fn go(app: &mut App, page: Page) -> Task<Message> {
    let changed = app.page != page;
    app.page = page;
    app.fold(Some(page.section()));
    if !changed {
        return Task::none();
    }
    app.flash = None;
    iced::widget::operation::snap_to(SCROLL, iced::widget::scrollable::RelativeOffset::START)
}

impl App {
    /// Expand `open` and fold every other section. The sidebar's layout
    /// changes at once (`nav_section_at`); only the opening section's reveal
    /// moves, under the default motion. A folded section's reveal goes to 0
    /// at once, so it opens from the top again next time.
    fn fold(&mut self, open: Option<Section>) {
        let open = open.filter(|s| s.expands());
        self.open = open;
        let now = Instant::now();
        for (s, f) in &mut self.folds {
            if Some(*s) == open {
                f.set_target(1.0, now);
            } else {
                f.snap(0.0);
            }
        }
    }

    /// Whether the sidebar or a lit row is moving, so the frame clock runs.
    fn nav_moving(&self) -> bool {
        self.flash.is_some()
            || self.cleared.is_some_and(|(_, ok)| ok)
            || self.folds.values().any(Animated::animating)
    }

    /// How lit row `path` is: 1 just revealed, falling to 0 over `REVEAL`.
    pub(crate) fn lit(&self, path: &str) -> f32 {
        match &self.flash {
            Some((p, at)) if p == path => {
                let t = at.elapsed().as_secs_f32() / REVEAL.as_secs_f32();
                // Held, then let go: the first third at full, the rest an
                // ease out, so the eye has time to land before it fades.
                let fade = ((t - 1.0 / 3.0) * 1.5).clamp(0.0, 1.0);
                1.0 - ec_ui::motion::ease_out(fade)
            }
            _ => 0.0,
        }
    }
}

/// Where a widget sits in the content column: the y offset of the
/// container with `target`'s id from the top of the content scrollable's
/// content, which is what `scroll_to` takes. Layout bounds are absolute and
/// unscrolled, so the difference is the offset whatever the scroll is now.
struct Measure {
    target: iced::widget::Id,
    scroll: iced::widget::Id,
    top: Option<f32>,
    at: Option<f32>,
}

impl Measure {
    fn new(target: iced::widget::Id) -> Self {
        Measure {
            target,
            scroll: iced::widget::Id::new(SCROLL),
            top: None,
            at: None,
        }
    }
}

impl iced::advanced::widget::Operation<Option<f32>> for Measure {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<Option<f32>>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&iced::widget::Id>, bounds: iced::Rectangle) {
        if id == Some(&self.target) {
            self.at = Some(bounds.y);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        _bounds: iced::Rectangle,
        content: iced::Rectangle,
        _translation: iced::Vector,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if id == Some(&self.scroll) {
            self.top = Some(content.y);
        }
    }

    fn finish(&self) -> iced::advanced::widget::operation::Outcome<Option<f32>> {
        iced::advanced::widget::operation::Outcome::Some(self.top.zip(self.at).map(|(top, at)| at - top))
    }
}

/// A second connection on its own thread, forwarding events into the runtime.
/// iced's `time::every` needs a tokio or smol backend and we enable neither,
/// so the wait lives in a thread rather than in a futures timer.
pub fn subscription(app: &App) -> Subscription<Message> {
    let mut subs = vec![blur(), events(), crate::search_ui::keys(&app.search)];
    if app.nav_moving() {
        subs.push(iced::window::frames().map(Message::NavFrame));
    }
    // The status feed runs only while its pane is showing.
    if app.page == Page::Network {
        subs.push(network::feed());
    }
    if app.page.section() == Section::Taskbar {
        subs.push(crate::tray::feed());
        // While results show, the arrows and Escape are the search's.
        if !app.search.active() {
            subs.push(taskbar_keys());
        }
        // The frame clock runs only while something in the picture moves.
        if app.bar.animating() {
            subs.push(iced::window::frames().map(|t| Message::Bar(crate::taskbar::Msg::Frame(t))));
        }
    }
    if app.page == Page::Animations {
        subs.push(iced::window::frames().map(|t| Message::Anim(crate::animations::Msg::Frame(t))));
    }
    // The account list is re-read only while its pane shows.
    if app.page == Page::Accounts && app.accounts.polls() {
        subs.push(crate::accounts::poll());
    }
    Subscription::batch(subs)
}

/// A press anywhere is the only focus-loss signal available: iced 0.14's
/// `text_input` has no blur hook. `NumberBlur` is a no-op unless a draft is
/// open and actually differs from the live value.
fn blur() -> Subscription<Message> {
    iced::event::listen_with(|event, _status, _window| match event {
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_))
        | iced::Event::Touch(iced::touch::Event::FingerPressed { .. }) => Some(Message::NumberBlur),
        _ => None,
    })
}

/// The Taskbar pane's keyboard: arrows move the selected widget (or tray
/// entry), Delete takes it off, Escape drops a drag. Only keys nothing else
/// took; the pane then asks the tree whether a text field is focused, since
/// iced 0.14's `text_input` lets Up and Down through uncaptured.
fn taskbar_keys() -> Subscription<Message> {
    use crate::taskbar::{Msg, Stroke as K};
    use iced::keyboard::{key::Named, Event, Key};
    iced::event::listen_with(|event, status, _window| {
        let iced::Event::Keyboard(Event::KeyPressed { key, modifiers, .. }) = event else {
            return None;
        };
        if status == iced::event::Status::Captured || !modifiers.is_empty() {
            return None;
        }
        let k = match key {
            Key::Named(Named::ArrowLeft) => K::Left,
            Key::Named(Named::ArrowRight) => K::Right,
            Key::Named(Named::ArrowUp) => K::Up,
            Key::Named(Named::ArrowDown) => K::Down,
            Key::Named(Named::Delete | Named::Backspace) => K::Delete,
            Key::Named(Named::Escape) => K::Escape,
            _ => return None,
        };
        Some(Message::Bar(Msg::Key(k)))
    })
}

fn events() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(32, async move |mut sender| {
            std::thread::spawn(move || {
                let mut client = None;
                loop {
                    if client.is_none() {
                        if let Ok(mut c) = ec_ipc::Client::connect() {
                            if c.subscribe(&[EventKind::Output, EventKind::ConfigError, EventKind::Config])
                                .is_ok()
                            {
                                client = Some(c);
                            }
                        }
                    }
                    if let Some(c) = client.as_mut() {
                        loop {
                            match c.poll_event() {
                                Ok(Some(ev)) => {
                                    if sender.try_send(Message::Wire(ev.kind, ev.data)).is_err() {
                                        return;
                                    }
                                }
                                Ok(None) => break,
                                Err(_) => {
                                    client = None;
                                    break;
                                }
                            }
                        }
                    }
                    std::thread::sleep(POLL);
                }
            });
        })
    })
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    // The frame is the only thing that reads the window's width; every row
    // below folds on its own.
    iced::widget::responsive(move |size| frame(app, Density::for_width(size.width))).into()
}

/// The sidebar's section tree: what the rail shows when nothing is searched.
fn tree(app: &App, density: Density) -> Vec<Element<'_, Message, Theme>> {
    Section::ALL
        .iter()
        .map(|s| {
            let current = app.page.section() == *s;
            if !s.expands() {
                return nav_item_at(density, s.title(), current, Message::Open(s.first()));
            }
            let pages = s
                .pages()
                .iter()
                .map(|p| nav_page_at(density, p.title(), *p == app.page, Message::Open(*p)))
                .collect();
            let reveal = app.folds.get(s).map_or(0.0, Animated::value);
            nav_section_at(
                density,
                s.title(),
                current,
                app.open == Some(*s),
                reveal,
                Message::Section(*s),
                pages,
            )
        })
        .collect()
}

fn frame(app: &App, density: Density) -> Element<'_, Message, Theme> {
    let mut nav = vec![crate::search_ui::field(app, density)];
    if app.search.active() {
        nav.extend(crate::search_ui::results(app));
    } else {
        nav.extend(tree(app, density));
    }

    let mut footer = Vec::new();
    if app.restart_pending {
        footer.push(("restart", "required".into()));
    }

    let mut controls = vec![pill("Reload", false, Message::Reload)];
    match app.page.section() {
        Section::Network => {
            let (state, measure) = app.net.chip();
            controls.push(status_chip(&state, &measure));
        }
        Section::Display => {
            let (state, measure) = display::status(app);
            controls.push(status_chip(&state, &measure));
        }
        Section::Taskbar => {
            let (state, measure) = crate::taskbar::status(app);
            controls.push(status_chip(&state, &measure));
        }
        Section::Addons => {
            let (state, measure) = crate::addons::status(app);
            controls.push(status_chip(&state, &measure));
        }
        Section::OracleEyes => {
            let (state, measure) = crate::oracle::status(app);
            controls.push(status_chip(&state, &measure));
        }
        Section::Accounts => {
            let (state, measure) = crate::accounts::status(app);
            controls.push(status_chip(&state, &measure));
        }
        _ if app.page == Page::Animations => {
            let (state, measure) = crate::animations::status(app);
            controls.push(status_chip(&state, &measure));
        }
        _ => {}
    }
    let mut blocks = vec![header(
        app.page.title(),
        subtitle(app.page.subtitle(), "", ""),
        controls,
    )];
    if let Some(problem) = &app.banner {
        blocks.push(banner(problem, app.glass_radius));
    }
    match app.page {
        Page::Display => blocks.extend(display::blocks(app)),
        Page::Network => blocks.extend(network::blocks(&app.net, app.glass_radius)),
        Page::Addons => blocks.extend(crate::addons::blocks(app)),
        // A status grid, then the rows: two silhouettes, not two panels of rows.
        Page::OeGeneral => {
            blocks.push(crate::oracle::hero(app));
            blocks.push(schema_page(app));
        }
        Page::Animations => blocks.extend(crate::animations::blocks(app)),
        Page::Accounts => blocks.extend(crate::accounts::blocks(app)),
        p if p.section() == Section::Taskbar => blocks.extend(crate::taskbar::blocks(app, p)),
        _ => blocks.push(schema_page(app)),
    }

    // The window is transparent (`main.rs`): with blur on, sidebar and
    // content are one pane of smoked glass a step apart in density; with it
    // off both fall back to opaque grounds.
    row![
        sidebar_at(density, app.blur, nav, footer),
        iced::widget::container(
            // A thin rail, inset top and bottom so it reads as a hint, not a
            // full-height bar.
            iced::widget::container(
                scrollable(content_at(density, blocks))
                    .id(SCROLL)
                    .direction(scrollable::Direction::Vertical(
                        scrollable::Scrollbar::new().width(4).scroller_width(4).margin(3),
                    ))
                    .style(theme::eclipse_scrollable)
                    .height(Length::Fill),
            )
            .padding([space::BLOCK, 0.0]),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::content_ground(app.blur)),
    ]
    .into()
}

/// The two error shapes every DE app renders identically: a denial and a
/// config error. Neither is ever swallowed.
fn banner(problem: &Problem, radius: f32) -> Element<'_, Message, Theme> {
    panel(
        radius,
        column![
            row![
                mono(&problem.headline()),
                Space::new().width(Length::Fill),
                pill("Dismiss", false, Message::Dismiss),
            ]
            .align_y(iced::Alignment::Center),
            hairline(),
            iced::widget::text(problem.detail().to_string())
                .font(ec_ui::tokens::font::UI)
                .size(ec_ui::tokens::size::BODY_SMALL)
                .style(theme::text_secondary),
        ]
        .spacing(space::ROW_Y),
    )
    .into()
}

/// Every schema key this page claims, in one panel: grouped by
/// `place_for`'s group, in schema order — except a moded node's keys
/// (`schema::moded`), which follow their `mode` picker in runs, one per set
/// of modes they apply to, in mode order. A run the current mode does not
/// use is dimmed, and stays editable.
///
/// One panel, not one per group: a page is a short list, and a stack of
/// same-shaped cards is the silhouette COMPOSITION.md rules out. Groups are
/// a hairline and a micro label inside it, and the label is dropped when the
/// page has one group — the page title already says what it is.
fn schema_page(app: &App) -> Element<'_, Message, Theme> {
    let mut groups: Vec<(&str, Vec<Element<'_, Message, Theme>>)> = Vec::new();
    let mode_path = format!("{}.mode", crate::schema::BLUR);
    let mode_key = app.key(&mode_path);
    let modes: &[String] = match mode_key.map(|k| &k.control) {
        Some(Control::Segmented(v) | Control::Dropdown(v)) => v,
        _ => &[],
    };
    let current = mode_key.and_then(|k| k.value.as_str()).unwrap_or_default();
    // (the modes a run applies to, its rows), in first-seen order.
    let mut runs: Vec<(Vec<&str>, Vec<&Key>)> = Vec::new();
    let mut runs_group = None;

    let shown = |k: &&Key| page_of(&k.path) == Some(app.page);
    let mut keys: Vec<&Key> = app.rows.iter().filter(shown).collect();
    // The terminal command decides whether terminal apps are offered at
    // all: it goes directly above the switch it gates.
    if let Some(from) = keys.iter().position(|k| k.path == TERMINAL) {
        let gated = keys.remove(from);
        let to = keys
            .iter()
            .position(|k| k.path == TERMINAL_APPS)
            .unwrap_or(keys.len());
        keys.insert(to, gated);
    }
    let mut set_apart = false;
    for key in keys {
        let group = place_for(&key.path).map_or("", |p| p.group);
        let rows = match groups.iter().position(|(g, _)| *g == group) {
            Some(i) => &mut groups[i].1,
            None => {
                groups.push((group, Vec::new()));
                &mut groups.last_mut().expect("just pushed").1
            }
        };
        // An unset mirror would be a second slider reading the same number
        // as the key above it. It folds into one row that says so, and
        // offers to set it apart — hidden as a control, never as a setting.
        if hidden_mirror(key) {
            if !set_apart {
                set_apart = true;
                rows.push(list_row(
                    "Vertical gaps",
                    row![
                        iced::widget::text("Same as above")
                            .font(ec_ui::tokens::font::UI)
                            .size(ec_ui::tokens::size::BODY_SMALL)
                            .style(theme::text_tertiary),
                        pill("Set separately", false, Message::SetApart),
                    ]
                    .spacing(space::CONTROL_GAP)
                    .align_y(iced::Alignment::Center),
                ));
            }
            continue;
        }
        if let Some(applies) = crate::schema::moded(crate::schema::BLUR, &key.path, modes) {
            runs_group.get_or_insert(group);
            match runs.iter_mut().find(|(a, _)| *a == applies) {
                Some((_, keys)) => keys.push(key),
                None => runs.push((applies, vec![key])),
            }
            continue;
        }
        rows.push(setting_row(app, &key.label(), key));
        let blurb = match key.path.as_str() {
            "mode" => crate::schema::mode_blurb(key.value.as_str().unwrap_or_default()),
            p if p == mode_path => crate::schema::blur_blurb(current),
            "ui.show-key-hints" => Some("Keyboard hints in the launcher and start menu"),
            "launcher.search.frecency" => Some(FRECENCY_BLURB),
            "settings.search.frecency" => Some(FRECENCY_BLURB),
            TERMINAL_APPS => Some("Opened in the terminal command above."),
            crate::oracle::MODEL_COMMAND => Some(crate::oracle::approval_line(key)),
            "oracle-eyes.bind.select" => Some("A chord like Super+A. \"none\" leaves an action unbound."),
            "oracle-eyes.debug" => Some("While on, the taskbar eye turns red and shows in screen captures."),
            _ => None,
        };
        if let Some(blurb) = blurb {
            // Inset like `list_row`'s label, so it reads as that row's caption.
            rows.push(
                iced::widget::container(
                    iced::widget::text(blurb)
                        .font(ec_ui::tokens::font::UI)
                        .size(ec_ui::tokens::size::BODY_SMALL)
                        .style(theme::text_tertiary),
                )
                .padding([0.0, space::CARD])
                .into(),
            );
        }
    }

    // In mode order: a run goes where the first mode it applies to is.
    let first = |applies: &[&str]| {
        applies
            .first()
            .and_then(|m| modes.iter().position(|v| v == m))
            .unwrap_or(modes.len())
    };
    runs.sort_by_key(|(applies, _)| first(applies));
    if let Some(rows) = runs_group.and_then(|g| groups.iter_mut().find(|(n, _)| *n == g)) {
        for (applies, keys) in runs {
            let dim = !applies.contains(&current);
            let mut caption = applies
                .iter()
                .map(|m| crate::schema::value_label(m))
                .collect::<Vec<_>>()
                .join(" · ");
            // Said in words as well as in strength: on glass the controls
            // cannot be faded (`dimmed_at`), only the labels.
            if dim && !caption.is_empty() {
                caption.push_str(" · not in use");
            }
            // The divider stops where the rows' labels do, so a run reads as a
            // softer break inside the panel rather than a rule across it.
            let divider = iced::widget::container(hairline()).padding([0.0, space::CARD]);
            let mut run = Column::new().push(divider).spacing(space::ROW_Y);
            if !caption.is_empty() {
                run = run.push(row_caption(&caption));
            }
            for key in keys {
                run = run.push(setting_row_at(app, &key.label(), key, dim && app.blur));
            }
            rows.1.push(dimmed_at(run, dim, app.blur));
        }
    }

    // The history these switches govern is cleared from the page that holds
    // either one, under the last of its rows.
    if app
        .rows
        .iter()
        .any(|k| is_frecency(&k.path) && page_of(&k.path) == Some(app.page))
    {
        if let Some((_, rows)) = groups.last_mut() {
            rows.push(hairline());
            rows.push(clear_usage_row(app.cleared));
        }
    }

    if groups.is_empty() {
        return edge_note(
            "Nothing to set here yet",
            "This compositor reports no settings for this page. A newer abyss adds them; until then \
             they are not in abyss.kdl either.",
            color::NEUTRAL,
        );
    }
    let labelled = groups.len() > 1;
    let mut col = Column::new().spacing(space::ROW_Y);
    for (i, (group, rows)) in groups.into_iter().enumerate() {
        if i > 0 {
            col = col.push(hairline());
        }
        if labelled {
            col = col.push(micro_label(group));
        }
        for r in rows {
            col = col.push(r);
        }
    }
    panel(app.glass_radius, col).into()
}

const FRECENCY_BLURB: &str =
    "History stays on this device and stores no search text, only what was opened and when.";

fn is_frecency(path: &str) -> bool {
    matches!(path, "launcher.search.frecency" | "settings.search.frecency")
}

/// "Usage history" with its one action. Pressed, the button gives way to a
/// plain "Cleared" (or the failure) for a moment: confirmed in place, no modal,
/// since the history is only a ranking hint and rebuilds itself.
fn clear_usage_row<'a>(cleared: Option<(Instant, bool)>) -> Element<'a, Message, Theme> {
    let control: Element<'a, Message, Theme> = match cleared {
        // Padded as the button is, so the row does not change height.
        Some((_, true)) => iced::widget::container(
            iced::widget::text("Cleared")
                .font(ec_ui::tokens::font::UI_MEDIUM)
                .size(ec_ui::tokens::size::BODY_SMALL)
                .style(theme::text_secondary),
        )
        .padding([space::PILL_Y, space::PILL_X])
        .into(),
        failed => {
            let label = if failed.is_some() {
                "Could not clear, retry"
            } else {
                "Clear usage history"
            };
            iced::widget::button(
                iced::widget::text(label)
                    .font(ec_ui::tokens::font::UI_MEDIUM)
                    .size(ec_ui::tokens::size::BODY_SMALL),
            )
            .padding([space::PILL_Y, space::PILL_X])
            .on_press(Message::ClearUsage)
            .style(theme::danger)
            .into()
        }
    };
    list_row("Usage history", control)
}

/// One setting's row: [`list_row`] in a container carrying [`row_id`], so
/// [`Message::Reveal`] can find it, and lit while a reveal fades. The
/// container is there lit or not, so the tree does not change shape as the
/// light goes.
pub(crate) fn setting_row<'a>(app: &'a App, label: &str, key: &'a Key) -> Element<'a, Message, Theme> {
    setting_row_at(app, label, key, false)
}

/// [`setting_row`] with its label stepped down, for a row that does not
/// apply right now on a translucent column (`dimmed_at`).
fn setting_row_at<'a>(app: &'a App, label: &str, key: &'a Key, dim: bool) -> Element<'a, Message, Theme> {
    let lit = app.lit(&key.path);
    let reason = app.invalid.get(&key.path).map_or("", String::as_str);
    iced::widget::container(column![
        list_row_at(label, control(app, key), dim),
        refusal(reason)
    ])
    .id(row_id(&key.path))
    .width(Length::Fill)
    .style(move |_t: &Theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(color::HIGHLIGHT.scale_alpha(lit))),
        border: iced::Border {
            color: color::BORDER_STRONG.scale_alpha(lit),
            width: space::HAIRLINE,
            radius: radius::INSET.into(),
        },
        ..iced::widget::container::Style::default()
    })
    .into()
}

/// Why the compositor refused a draft, in its own words, under the row:
/// "Super+Escape is reserved", "unknown key 'Foo'". The "invalid" beside the
/// field says that it was refused; this says why. Always in the tree, and no
/// height while there is nothing to say, so the field keeps its focus and an
/// accepted row is no taller.
fn refusal<'a>(reason: &str) -> Element<'a, Message, Theme> {
    let line = iced::widget::container(
        iced::widget::text(reason.to_owned())
            .font(font::UI)
            .size(size::BODY_SMALL)
            .style(theme::text_danger),
    );
    if reason.is_empty() {
        // `max_height`, not `height(Fixed(0.0))`: iced's column drops a
        // child sized `Fixed(0)` from the tree altogether.
        line.max_height(0.0).into()
    } else {
        // Inset like the row's label, and the row's own air beneath it.
        line.padding(
            iced::Padding::ZERO
                .left(space::CARD)
                .right(space::CARD)
                .bottom(space::ROW_Y),
        )
        .into()
    }
}

/// An unset key that mirrors another (`Key::mirror_of`) and can be written:
/// its row is folded into the "set separately" affordance. A locked one
/// stays a read-only row, since the affordance could not act on it.
fn hidden_mirror(key: &Key) -> bool {
    key.mirror_of().is_some() && key.value.is_null() && !key.locked()
}

/// The one place a schema type becomes a widget.
pub(crate) fn control<'a>(app: &'a App, key: &'a Key) -> Element<'a, Message, Theme> {
    let path = key.path.clone();
    if key.locked() {
        // Policy-owned, or the compositor says not writable. Shown, never
        // editable by any path in this app: the policy editor owns it.
        return match &key.control {
            Control::Toggle => Toggle::locked(key.as_bool()).into(),
            _ => mono(&key.display()),
        };
    }

    match &key.control {
        Control::Toggle => Toggle::new(key.as_bool(), move |on| Message::Toggled(path.clone(), on)).into(),

        Control::Slider {
            min,
            max,
            integral,
            drag_max,
        } => {
            let current = app
                .live
                .get(&key.path)
                .copied()
                .unwrap_or_else(|| app.effective(key));
            let span = Span {
                min: *min,
                max: *max,
                integral: *integral,
            };
            // The knob drags across `drag_max` (fine-grained), but a typed
            // draft above it still validates against the real `max` via
            // `span` — `drag_max` narrows the track, not the value.
            let drag_max = drag_max.unwrap_or(*max);
            let id = Num::Key(key.path.clone());
            let draft = app.nums.get(&id);
            let shown = draft.cloned().unwrap_or_else(|| span.format(current));
            let invalid = draft.is_some_and(|d| span.parse(d).is_none());
            let typed = id.clone();
            let released = path.clone();
            NumericSlider::new(
                *min..=drag_max,
                current,
                shown,
                move |v| Message::SliderMoved(path.clone(), v),
                move |t| Message::NumberTyped(typed.clone(), t),
            )
            .step(if *integral { 1.0 } else { 0.01 })
            .on_release(Message::SliderReleased(released))
            .on_commit(Message::NumberCommitted(id))
            .invalid(invalid)
            .unit(key.unit())
            .into()
        }

        Control::Segmented(values) => {
            let current = key.value.as_str().unwrap_or_default().to_string();
            pill_group(
                values
                    .iter()
                    .map(|v| {
                        pill(
                            crate::schema::value_label(v),
                            *v == current,
                            Message::Chose(path.clone(), v.clone()),
                        )
                    })
                    .collect(),
            )
        }

        Control::Dropdown(values) => {
            pick_list(values.clone(), key.value.as_str().map(str::to_owned), move |v| {
                Message::Chose(path.clone(), v)
            })
            .into()
        }

        Control::Text { color } => {
            let draft = app.drafts.get(&key.path);
            // An unset key is an empty field: the dash a read-only value
            // shows is the placeholder here, never text that can be typed
            // after. A default, when there is one, says more than the dash.
            let shown = draft.cloned().unwrap_or_else(|| key.text());
            let hint = key.default.as_str().filter(|d| !d.is_empty()).unwrap_or("—");
            let submit = path.clone();
            let invalid = app.invalid.contains_key(&key.path);
            let mut input = text_input(hint, &shown)
                .on_input(move |t| Message::Edited(path.clone(), t))
                // The row label's face and size: the field is part of the
                // row, not a different voice beside it.
                .font(font::UI)
                .size(size::BODY)
                .style(if invalid {
                    theme::eclipse_input_invalid
                } else {
                    theme::eclipse_input
                });
            // A rejected draft has no commit path at all, rather than a
            // commit that fails after the fact.
            if !invalid {
                input = input.on_submit(Message::Committed(submit));
            }
            // One tree in both states: the verdict slot is always there,
            // empty while the draft is good. Swapping the row in and out
            // rebuilt the input and dropped keyboard focus mid-typing.
            let field = verdict_row(input, invalid);
            // `FIELD_W` when there is room, the row's width when there is not.
            let field = iced::widget::container(field)
                .width(Length::Fill)
                .max_width(space::FIELD_W + space::CONTROL_GAP + space::VERDICT_W);
            if key.path == "wallpaper.path" {
                return row![
                    field,
                    pill("Browse\u{2026}", false, Message::Browse(key.path.clone()))
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(iced::Alignment::Center)
                .into();
            }
            // A colour key shows the colour it holds now — what was written,
            // not the draft — as a swatch that opens its picker under the row.
            let Some([r, g, b, a]) = color.then(|| crate::schema::rgba(&key.display())).flatten() else {
                return field.into();
            };
            let open = app.picker.as_deref() == Some(key.path.as_str());
            let head = iced::widget::container(
                row![
                    swatch_button(
                        iced::Color::from_rgba8(r, g, b, f32::from(a) / 255.0),
                        open,
                        Message::Picker(key.path.clone()),
                    ),
                    field
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(iced::Alignment::Center),
            )
            // The field's own cap, plus the swatch beside it.
            .width(Length::Fill)
            .max_width(
                space::FIELD_W
                    + space::CONTROL_GAP
                    + space::VERDICT_W
                    + space::CONTROL_GAP
                    + space::SWATCH
                    + 2.0 * space::SWATCH_PAD,
            );
            if !open {
                return head.into();
            }
            // The picker follows the draft while one is good, so a hex typed
            // into the field moves the knobs too.
            let text = app
                .drafts
                .get(&key.path)
                .filter(|_| !invalid)
                .cloned()
                .unwrap_or_else(|| key.display());
            let rgba = crate::schema::rgba(&text).unwrap_or([r, g, b, a]);
            // Alpha only where the value spells one: eight hex digits.
            let alpha = text.len() == 9;
            let (moved, released) = (key.path.clone(), key.path.clone());
            column![
                head,
                color_picker(
                    rgba,
                    alpha,
                    move |c| Message::Edited(moved.clone(), crate::schema::hex(c, alpha)),
                    Message::Committed(released),
                ),
                mono(&crate::schema::hex(rgba, alpha)),
            ]
            .spacing(space::PICKER_GAP)
            .into()
        }

        // `set_config_value` writes one scalar at a dotted path; a list needs
        // the node editor. Shown so the setting is never hidden.
        // A string list is shown, not edited — except an argv a line can
        // hold, which edits as one: words split on spaces.
        Control::List => match key
            .argv_text()
            .filter(|_| key.path == crate::oracle::MODEL_COMMAND)
        {
            Some(line) => argv_field(app, key, line),
            None => mono(&key.display()),
        },
    }
}

/// The model command's argv as one editable line: the text field every text
/// key uses, its draft written back as a list (`App::typed`).
fn argv_field<'a>(app: &'a App, key: &'a Key, line: String) -> Element<'a, Message, Theme> {
    let path = key.path.clone();
    let shown = app.drafts.get(&key.path).cloned().unwrap_or(line);
    let invalid = app.invalid.contains_key(&key.path);
    let mut input = text_input("program and arguments", &shown)
        .on_input(move |t| Message::Edited(path.clone(), t))
        .font(font::DATA)
        .size(size::MONO)
        .style(if invalid {
            theme::eclipse_input_invalid
        } else {
            theme::eclipse_input
        });
    if !invalid {
        input = input.on_submit(Message::Committed(key.path.clone()));
    }
    iced::widget::container(verdict_row(input, invalid))
        .width(Length::Fill)
        .max_width(space::FIELD_W + space::CONTROL_GAP + space::VERDICT_W)
        .into()
}

/// A text field and the slot that says whether its draft is refused. The
/// slot is always in the tree and always `VERDICT_W` wide, so the widget
/// tree is the same shape in both states and iced keeps the input's focus.
fn verdict_row<'a>(
    input: iced::widget::TextInput<'a, Message, Theme>,
    invalid: bool,
) -> Element<'a, Message, Theme> {
    row![
        input,
        iced::widget::container(mono(if invalid { "invalid" } else { "" }))
            .width(Length::Fixed(space::VERDICT_W))
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(iced::Alignment::Center)
    .into()
}

const INSET_SPAN: Span = Span {
    min: 0.0,
    max: INSET_MAX,
    integral: true,
};

/// The scale control: one row's worth, lifted out because the output card's
/// `column!` is already the widest expression in this file.
fn scale_control(app: &App, id: u64, scale: f64) -> Element<'_, Message, Theme> {
    let span = Span {
        min: SCALE_MIN,
        max: SCALE_MAX,
        integral: false,
    };
    let num = Num::Scale(id);
    let draft = app.nums.get(&num);
    let shown = draft.cloned().unwrap_or_else(|| span.format(scale));
    let invalid = draft.is_some_and(|d| span.parse(d).is_none());
    let typed = num.clone();
    NumericSlider::new(
        SCALE_MIN..=SCALE_MAX,
        scale,
        shown,
        move |v| Message::OutputScale(id, v),
        move |t| Message::NumberTyped(typed.clone(), t),
    )
    .step(SCALE_STEP)
    .on_release(Message::OutputScaleReleased(id))
    .on_commit(Message::NumberCommitted(num))
    .invalid(invalid)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: Span = Span {
        min: SCALE_MIN,
        max: SCALE_MAX,
        integral: false,
    };

    /// Tag and child count, all the way down: what iced's diff compares to
    /// decide whether a widget's state (the input's focus) survives.
    fn shape(t: &iced::advanced::widget::Tree) -> String {
        let kids: Vec<String> = t.children.iter().map(shape).collect();
        format!("{:?}[{}]", t.tag, kids.join(","))
    }

    #[test]
    fn a_text_field_keeps_its_tree_shape_across_validity() {
        let tree = |invalid| {
            let el = verdict_row(text_input("", "#f0"), invalid);
            shape(&iced::advanced::widget::Tree::new(&el))
        };
        assert_eq!(tree(false), tree(true));
    }

    /// The refusal line under a row comes and goes without changing the
    /// row's tree, so a field being typed into keeps its focus.
    #[test]
    fn a_refusal_keeps_the_rows_tree_shape() {
        let tree = |reason: &str| {
            let el: Element<'_, Message, Theme> = iced::widget::column![
                verdict_row(text_input("", "x"), !reason.is_empty()),
                refusal(reason)
            ]
            .into();
            shape(&iced::advanced::widget::Tree::new(&el))
        };
        assert_eq!(
            tree(""),
            tree("Super+Escape is reserved (COMP-04 §6) and cannot be bound")
        );
    }

    /// A section folding and unfolding keeps its head and its well in place,
    /// so the head's button keeps a press that spans the change. (Only the
    /// chevron inside the head is redrawn as another shape.)
    #[test]
    fn a_sidebar_section_keeps_its_tree_shape_open_or_folded() {
        let tree = |open: bool| {
            let pages = vec![nav_page_at(Density::Regular, "A", false, Message::Reload)];
            let el = nav_section_at(Density::Regular, "S", false, open, 0.5, Message::Reload, pages);
            let t = iced::advanced::widget::Tree::new(&el);
            let head = &t.children[0];
            let button = &head.children[1];
            let well = &t.children[1];
            (
                t.children.len(),
                format!("{:?}{:?}", head.tag, button.tag),
                shape(well),
            )
        };
        assert_eq!(tree(false), tree(true));
    }

    #[test]
    fn a_typed_number_is_clamped_to_the_sliders_range() {
        assert_eq!(INSET_SPAN.parse("999"), Some(INSET_MAX));
        assert_eq!(INSET_SPAN.parse("-4"), Some(0.0));
        assert_eq!(SCALE.parse("9"), Some(SCALE_MAX));
    }

    /// `drag_max` narrows the knob's own span, but the typed-input path
    /// (`App::span`, and so `commit_num`'s clamp) must still validate against
    /// the schema's real `max` — a drag stays fine-grained, a typed number
    /// still reaches the true limit.
    #[test]
    fn a_typed_number_reaches_the_true_max_even_when_the_drag_span_is_narrower() {
        let mut app = App::new();
        app.rows.clear();
        app.rows.push(Key {
            path: "decoration.rounding".into(),
            file: crate::schema::File::Abyss,
            value: json!(4),
            default: json!(4),
            source: None,
            readable: true,
            writable: true,
            doc: String::new(),
            needs_restart: false,
            approval: None,
            control: Control::Slider {
                min: 0.0,
                max: 64.0,
                integral: true,
                drag_max: Some(16.0),
            },
        });

        let id = Num::Key("decoration.rounding".to_string());
        let span = app.span(&id).expect("a slider key has a span");
        assert_eq!(span.max, 64.0, "typed validation ignores drag_max");
        assert_eq!(span.parse("999"), Some(64.0));
    }

    #[test]
    fn a_typed_number_that_does_not_parse_is_refused() {
        for bad in ["", "  ", "2x", "nan", "inf", "--1"] {
            assert_eq!(INSET_SPAN.parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn surrounding_space_is_not_a_refusal() {
        assert_eq!(INSET_SPAN.parse(" 42 "), Some(42.0));
    }

    #[test]
    fn a_reading_round_trips_through_the_entry() {
        for v in [0.0, 1.0, 37.0, INSET_MAX] {
            assert_eq!(INSET_SPAN.parse(&INSET_SPAN.format(v)), Some(v));
        }
        for v in [SCALE_MIN, 1.0, 1.25, SCALE_MAX] {
            assert_eq!(SCALE.parse(&SCALE.format(v)), Some(v));
        }
    }

    #[test]
    fn the_overscan_bound_is_the_uis_own() {
        assert_eq!(INSET_MAX, 120.0);
    }
}
