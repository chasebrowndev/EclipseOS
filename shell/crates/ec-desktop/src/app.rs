// SPDX-License-Identifier: AGPL-3.0-only
//! The daemon: one Bottom-layer surface per output, kept in step with the
//! compositor's output list, and the [`Desktop`] model they all draw.
//!
//! The output-following half is the wallpaper's (`ec_wallpaper::app`), and
//! kept deliberately the same shape: a pure [`reconcile`] over the connector
//! names, a settle delay for a freshly hotplugged output, and an empty list
//! read as "no answer" rather than "every monitor went away".
//!
//! **How a view gets updates.** `App::desktop` is the model: `entries`,
//! `icons`, `layout`, `failure`. Anything that changes it arrives as a
//! [`Message`], so iced redraws every surface after it: [`Message::Rescan`]
//! when the folder changed on disk (the watcher polls `scan::fingerprint`),
//! [`Message::Reconfigured`] when the config or theme moved, and
//! [`Message::Do`] for an operation the view asked for. The view never
//! touches the disk.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ec_ipc::{Client, EventKind};
use iced::window::Id;
use iced::{Size, Subscription, Task};
use iced_layershell::to_layer_message;
use serde_json::{json, Value};

use crate::desktop::{Desktop, Failure, Op};
use crate::float::{self, Float};
use crate::scan;
use crate::ui::{Act, Key, Pointer, Scene, Ui, Verb};

/// How long an output seen for the first time waits before it gets a surface.
pub const SETTLE: Duration = Duration::from_millis(250);

/// How often the watcher retries a control socket that is not there.
const RETRY: Duration = Duration::from_secs(1);

/// What changes what the desktop draws.
const KINDS: &[EventKind] = &[EventKind::Output, EventKind::Config];

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    /// The output list may have moved; re-read it.
    Refresh,
    /// A settling output has waited out [`SETTLE`].
    Screens,
    /// A config reload succeeded; re-read `misc.terminal-command`.
    Reconfigured,
    /// The desktop folder may have changed on disk.
    Rescan,
    /// An operation from the view. Its result lands in `App::desktop`
    /// (`failure`, `entries`, `layout`).
    Do(Op),
    /// The pointer did something on a surface.
    Pointer(Id, Pointer),
    /// A key the desktop acts on, with whether Ctrl was down.
    Key(Key, bool),
    /// Ctrl went down or up.
    Ctrl(bool),
    /// A row of the context menu was clicked.
    Chose(Verb),
    /// The rename field changed, or Enter was pressed in it.
    RenameInput(String),
    RenameSubmit,
    /// The launch prompt's buttons.
    Allow,
    KeepLocked,
    /// A press on the clear ground around the menu or prompt: close it.
    DismissFloating,
    /// A click on the failure note, or its time ran out.
    DismissToast,
    ToastExpired(u64),
    /// A click on a floating sheet that does nothing but must not fall
    /// through to the desktop under it.
    Swallow,
    /// A surface's logical size, from which the view derives its grid.
    Sized(Id, Size),
    /// The compositor took a surface away — its output went, most likely.
    Closed(Id),
}

/// One surface: the connector it covers, and its size once known.
pub struct Surface {
    pub output: String,
    pub size: Option<Size>,
}

pub struct App {
    client: Option<Client>,
    pub surfaces: HashMap<Id, Surface>,
    seen: HashMap<String, Instant>,
    checking: bool,
    pub desktop: Desktop,
    /// Selection, drag, menu, rename and the rest of what the pointer did.
    pub ui: Ui,
    /// The surface the pointer last acted on: where the menu, a drag and the
    /// rename field are drawn.
    pub active: Option<Id>,
    /// The surface that has asked for the keyboard, if any.
    keys_on: Option<Id>,
    /// The Overlay surface carrying the context menu or the launch prompt.
    pub sheet: Option<Float>,
    /// The Overlay surface carrying the failure note.
    pub note: Option<Float>,
}

impl App {
    fn ensure(&mut self) {
        if self.client.is_none() {
            self.client = Client::connect().ok();
        }
    }

    /// One call, fail-soft: a dropped socket clears the client so the next
    /// call reconnects.
    fn call(&mut self, method: &str, params: Value) -> Option<Value> {
        self.ensure();
        let client = self.client.as_mut()?;
        match client.call(method, params) {
            Ok(v) => Some(v),
            Err(e) => {
                if matches!(e, ec_ipc::Error::Connect(_) | ec_ipc::Error::Io(_)) {
                    self.client = None;
                }
                None
            }
        }
    }

    /// Every connector the compositor lists.
    fn outputs(&mut self) -> Vec<String> {
        let Some(v) = self.call("get_outputs", json!({})) else {
            return Vec::new();
        };
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("name").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }

    /// `misc.terminal-command`, as the launcher reads it (TERM-01).
    fn read_terminal(&mut self) {
        let reply = self.call("get_config", json!({ "path": "misc.terminal-command" }));
        self.desktop.term = reply.as_ref().and_then(|r| {
            r.get("keys")?
                .as_array()?
                .first()?
                .get("value")?
                .as_str()
                .map(str::to_owned)
        });
    }
}

pub fn boot() -> (App, Task<Message>) {
    let mut app = App {
        client: None,
        surfaces: HashMap::new(),
        seen: HashMap::new(),
        checking: false,
        desktop: Desktop::open(),
        ui: Ui::default(),
        active: None,
        keys_on: None,
        sheet: None,
        note: None,
    };
    #[cfg(debug_assertions)]
    crate::preview::apply(&mut app);
    app.read_terminal();
    let task = screens(&mut app, Duration::ZERO);
    (app, task)
}

/// The widget id of the inline rename field.
pub const RENAME_ID: &str = "desktop-rename";

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    let was = app.ui.rename.as_ref().map(|r| r.from.clone());
    let task = handle(app, message);
    let mut tasks = vec![task, sync_floats(app), sync_keys(app)];
    // A rename that just began takes the field's focus, all selected.
    let now = app.ui.rename.as_ref().map(|r| r.from.clone());
    if now.is_some() && now != was {
        tasks.push(iced::widget::operation::focus(RENAME_ID));
        tasks.push(iced::widget::operation::select_all(RENAME_ID));
    }
    Task::batch(tasks)
}

/// Ask for the keyboard while something on the desktop needs it, and give it
/// back after: the compositor hands a surface the keyboard the moment it goes
/// from `None` to `OnDemand`, and takes it back on the way down.
fn sync_keys(app: &mut App) -> Task<Message> {
    use iced_layershell::reexport::KeyboardInteractivity;
    // While the sheet is up it is the surface that holds the keyboard.
    let sheet_up = app.ui.menu.is_some() || app.ui.prompt.is_some();
    let want = (app.ui.wants_keys() && !sheet_up).then_some(app.active).flatten();
    if want == app.keys_on {
        return Task::none();
    }
    let mut tasks = Vec::new();
    if let Some(id) = app.keys_on.take() {
        if app.surfaces.contains_key(&id) {
            tasks.push(Task::done(Message::KeyboardInteractivityChange {
                id,
                keyboard_interactivity: KeyboardInteractivity::None,
            }));
        }
    }
    if want.is_none() {
        // Nobody sees a modifier release while unfocused.
        app.ui.ctrl = false;
    }
    if let Some(id) = want {
        tasks.push(Task::done(Message::KeyboardInteractivityChange {
            id,
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
        }));
    }
    app.keys_on = want;
    Task::batch(tasks)
}

/// Run what the interface asked for. Operations go through `Desktop::run`,
/// the one door; what they leave behind is `Ui::settle`'s.
fn perform(app: &mut App, acts: Vec<Act>) -> Task<Message> {
    let mut tasks = Vec::new();
    for act in acts {
        match act {
            Act::Run(op) => {
                let result = app.desktop.run(op.clone());
                // Shown by the note, once.
                app.desktop.failure = None;
                if let Some(seq) = app.ui.settle(&op, &result, &app.desktop.entries) {
                    tasks.push(expire(seq));
                }
            }
            Act::Terminal => {
                let dir = app.desktop.dir.clone();
                let ok = dir
                    .as_deref()
                    .is_some_and(|d| crate::launch::terminal_here(app.desktop.term.as_deref(), d).is_ok());
                if !ok {
                    tasks.push(notify(app, "terminal", "spawn"));
                }
            }
            Act::Settings => {
                if crate::launch::settings("desktop/wallpaper").is_err() {
                    tasks.push(notify(app, "settings", "spawn"));
                }
            }
        }
    }
    Task::batch(tasks)
}

fn notify(app: &mut App, op: &'static str, kind: &'static str) -> Task<Message> {
    expire(app.ui.notify(Failure { op, kind }))
}

fn expire(seq: u64) -> Task<Message> {
    after(
        Duration::from_millis(ec_ui::tokens::desktop::TOAST_MS),
        Message::ToastExpired(seq),
    )
}

/// The surface events are about: the one the pointer is on, else any.
fn surface_for(app: &App, id: Option<Id>) -> Option<(Id, String, Size)> {
    let pick = |id: &Id| {
        app.surfaces
            .get(id)
            .and_then(|s| Some((*id, s.output.clone(), s.size?)))
    };
    id.as_ref().and_then(pick).or_else(|| {
        let mut ids: Vec<&Id> = app.surfaces.keys().collect();
        ids.sort_by_key(|i| format!("{i:?}"));
        ids.into_iter().find_map(pick)
    })
}

/// `f` over the active surface's scene, then whatever it asks for done.
fn with_scene(app: &mut App, f: impl FnOnce(&mut Ui, &Scene) -> Vec<Act>) -> Task<Message> {
    let Some((id, output, size)) = surface_for(app, app.active) else {
        return Task::none();
    };
    app.active = Some(id);
    let acts = {
        let scene = Scene::new(&app.desktop, &output, size);
        f(&mut app.ui, &scene)
    };
    perform(app, acts)
}

fn handle(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Refresh => screens(app, SETTLE),
        Message::Screens => {
            app.checking = false;
            screens(app, SETTLE)
        }
        Message::Reconfigured => {
            app.read_terminal();
            // A launcher that wanted a terminal may parse now; `rescan` sees
            // that by comparing, and looks up icons only for what changed.
            app.desktop.rescan();
            app.ui.prune(&app.desktop.entries);
            Task::none()
        }
        Message::Rescan => {
            app.desktop.rescan();
            app.ui.prune(&app.desktop.entries);
            Task::none()
        }
        Message::Do(op) => perform(app, vec![Act::Run(op)]),
        Message::Pointer(id, event) => {
            // Moves on another surface do not take the interaction from the
            // one a menu or drag is on; a press does.
            let busy = app.ui.menu.is_some() || app.ui.drag.is_some() || app.ui.band.is_some();
            if app.active != Some(id) && (!busy || !matches!(event, Pointer::Moved(_))) {
                app.active = Some(id);
            }
            if app.active != Some(id) {
                return Task::none();
            }
            with_scene(app, |ui, scene| ui.pointer(scene, event, Instant::now()))
        }
        Message::Key(key, ctrl) => {
            app.ui.ctrl = ctrl;
            with_scene(app, |ui, scene| ui.key(scene, key))
        }
        Message::Ctrl(down) => {
            app.ui.ctrl = down;
            Task::none()
        }
        Message::Chose(verb) => with_scene(app, |ui, scene| ui.choose(scene, verb)),
        Message::RenameInput(text) => {
            app.ui.rename_input(text);
            Task::none()
        }
        Message::RenameSubmit => with_scene(app, |ui, scene| ui.rename_commit(scene)),
        Message::Allow => with_scene(app, |ui, scene| ui.allow(scene)),
        Message::KeepLocked => {
            app.ui.dismiss_prompt();
            Task::none()
        }
        Message::DismissToast => {
            app.ui.dismiss_toast();
            Task::none()
        }
        Message::ToastExpired(seq) => {
            app.ui.expire(seq);
            Task::none()
        }
        Message::DismissFloating => {
            app.ui.dismiss_floating();
            Task::none()
        }
        Message::Swallow => Task::none(),
        Message::Sized(id, size) => {
            if let Some(s) = app.surfaces.get_mut(&id) {
                s.size = Some(size);
                // Floating things that exist before any pointer has (the
                // debug fixture's) belong to the first surface.
                if app.active.is_none() {
                    app.active = Some(id);
                }
            }
            Task::none()
        }
        Message::Closed(id) => {
            if app.sheet.is_some_and(|f| f.id == id) {
                app.sheet = None;
            }
            if app.note.is_some_and(|f| f.id == id) {
                app.note = None;
            }
            if app.active == Some(id) {
                app.active = None;
                app.ui.abandon_gesture();
            }
            if app.keys_on == Some(id) {
                app.keys_on = None;
            }
            if app.surfaces.remove(&id).is_some() {
                return screens(app, SETTLE);
            }
            Task::none()
        }
        // The layer-shell actions `to_layer_message` added are the runtime's.
        _ => Task::none(),
    }
}

/// Open and close the menu/prompt sheet and the failure note so that they
/// exist exactly while the model has something to show. See [`float`].
fn sync_floats(app: &mut App) -> Task<Message> {
    let sheet_want = (app.ui.menu.is_some() || app.ui.prompt.is_some())
        .then(|| surface_for(app, app.active).map(|(id, ..)| id))
        .flatten();
    let note_want = app
        .ui
        .toast
        .is_some()
        .then(|| surface_for(app, app.active).map(|(id, ..)| id))
        .flatten();
    let mut tasks = Vec::new();
    for (kind, want) in [(Kind::Sheet, sheet_want), (Kind::Note, note_want)] {
        let slot = match kind {
            Kind::Sheet => &mut app.sheet,
            Kind::Note => &mut app.note,
        };
        let plan = float::plan(*slot, want);
        if let Some(id) = plan.close {
            *slot = None;
            tasks.push(Task::done(Message::RemoveWindow(id)));
        }
        if let (Some(over), Some(output)) = (
            plan.open,
            plan.open
                .and_then(|o| app.surfaces.get(&o))
                .map(|s| s.output.clone()),
        ) {
            let id = Id::unique();
            let slot = match kind {
                Kind::Sheet => &mut app.sheet,
                Kind::Note => &mut app.note,
            };
            *slot = Some(Float { id, over });
            tasks.push(open_float(kind, id, output));
        }
    }
    Task::batch(tasks)
}

#[derive(Clone, Copy)]
enum Kind {
    Sheet,
    Note,
}

/// An Overlay-layer surface above the windows. The sheet covers the same area
/// as the desktop surface (same anchors, same exclusive-zone rule) so a point
/// means the same on both; it holds the keyboard while it is open and hears
/// every press outside the menu. The note is only as big as itself, in the
/// corner, and takes no keyboard.
fn open_float(kind: Kind, id: Id, output: String) -> Task<Message> {
    use ec_ui::tokens::desktop;
    use iced_layershell::reexport::{
        Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
    };
    let (size, anchor, margin, keyboard_interactivity) = match kind {
        Kind::Sheet => (
            (0, 0),
            Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
            None,
            KeyboardInteractivity::OnDemand,
        ),
        Kind::Note => {
            let m = desktop::MARGIN as i32;
            (
                (desktop::TOAST_W as u32, desktop::TOAST_H as u32),
                Anchor::Bottom | Anchor::Right,
                Some((0, m, m, 0)),
                KeyboardInteractivity::None,
            )
        }
    };
    Task::done(Message::NewLayerShell {
        settings: NewLayerShellSettings {
            size: Some(size),
            layer: Layer::Overlay,
            anchor,
            exclusive_zone: Some(0),
            margin,
            keyboard_interactivity,
            output_option: OutputOption::OutputName(output),
            events_transparent: false,
            namespace: None,
        },
        id,
    })
}

/// What to do about the output list. See [`reconcile`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub open: Vec<String>,
    pub close: Vec<Id>,
    /// An output is still settling: look again after [`SETTLE`].
    pub wait: bool,
}

/// The pure half of following the outputs, as in hyperion: `open` is the
/// surfaces there are, `live` the compositor's list, `seen` each uncovered
/// output's first sighting. An empty `live` is no answer and changes nothing.
pub fn reconcile(
    open: &[(Id, &str)],
    live: &[String],
    seen: &mut HashMap<String, Instant>,
    now: Instant,
    settle: Duration,
) -> Plan {
    let mut plan = Plan::default();
    if live.is_empty() {
        return plan;
    }
    let listed = |name: &str| live.iter().any(|n| n == name);
    for (id, name) in open {
        if !listed(name) {
            plan.close.push(*id);
        }
    }
    seen.retain(|name, _| listed(name));
    for name in live {
        if open.iter().any(|(_, n)| n == name) || plan.open.contains(name) {
            seen.remove(name);
            continue;
        }
        let first = *seen.entry(name.clone()).or_insert(now);
        if now.saturating_duration_since(first) >= settle {
            seen.remove(name);
            plan.open.push(name.clone());
        } else {
            plan.wait = true;
        }
    }
    plan
}

/// Bring the surfaces in line with the outputs.
fn screens(app: &mut App, settle: Duration) -> Task<Message> {
    let live = app.outputs();
    let open: Vec<(Id, &str)> = app
        .surfaces
        .iter()
        .map(|(id, s)| (*id, s.output.as_str()))
        .collect();
    let plan = reconcile(&open, &live, &mut app.seen, Instant::now(), settle);
    let mut tasks = Vec::new();
    for id in plan.close {
        app.surfaces.remove(&id);
        if app.active == Some(id) {
            app.active = None;
            app.ui.abandon_gesture();
        }
        if app.keys_on == Some(id) {
            app.keys_on = None;
        }
        tasks.push(Task::done(Message::RemoveWindow(id)));
    }
    for name in plan.open {
        tasks.push(open_surface(app, name));
    }
    if plan.wait && !app.checking {
        app.checking = true;
        tasks.push(after(SETTLE, Message::Screens));
    }
    Task::batch(tasks)
}

/// A Bottom-layer surface (just above the wallpaper) covering `name`, inside every other surface's
/// exclusive zone: the icons keep clear of the bar, unlike the wallpaper under
/// it, which covers the whole output.
fn open_surface(app: &mut App, name: String) -> Task<Message> {
    use iced_layershell::reexport::{
        Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
    };
    let id = Id::unique();
    app.surfaces.insert(
        id,
        Surface {
            output: name.clone(),
            size: None,
        },
    );
    Task::done(Message::NewLayerShell {
        settings: NewLayerShellSettings {
            // Zero on both axes with all four edges anchored: the usable area.
            size: Some((0, 0)),
            // Bottom, not Background: the wallpaper is on Background, and two
            // clients on one layer stack by creation order, which nothing here
            // controls. A layer above is above whatever maps first.
            layer: Layer::Bottom,
            anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
            // 0: respect the exclusive zones of the bar and panels.
            exclusive_zone: Some(0),
            margin: None,
            // The icon view asks for more (rename text entry) when it needs it.
            keyboard_interactivity: KeyboardInteractivity::None,
            // By name: a null output lands on whichever monitor is focused.
            output_option: OutputOption::OutputName(name),
            events_transparent: false,
            namespace: None,
        },
        id,
    })
}

/// `message` after `delay`. A thread and a oneshot: the runtime has no timer
/// in this feature set.
fn after(delay: Duration, message: Message) -> Task<Message> {
    let (tx, rx) = iced::futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let _ = tx.send(());
    });
    Task::future(async move {
        let _ = rx.await;
        message
    })
}

pub fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::batch([
        compositor(),
        folder(),
        // Keep the palette on the compositor's theme (ADR 0084).
        ec_ui::ipc::theme_subscription(|| Message::Reconfigured),
        iced::window::close_events().map(Message::Closed),
        iced::event::listen_with(|event, status, id| match event {
            // A key the rename field took (Enter submits it) is not the
            // desktop's too; Escape is the exception, the field only lets go.
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => key_of(&key)
                .filter(|k| *k == Key::Escape || status == iced::event::Status::Ignored)
                .map(|k| Message::Key(k, modifiers.control())),
            iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(m)) => {
                Some(Message::Ctrl(m.control()))
            }
            iced::Event::Window(
                iced::window::Event::Opened { size, .. } | iced::window::Event::Resized(size),
            ) => Some(Message::Sized(id, size)),
            _ => None,
        }),
    ])
}

/// The desktop's reading of a key.
fn key_of(key: &iced::keyboard::Key) -> Option<Key> {
    use iced::keyboard::key::Named;
    use iced::keyboard::Key as K;
    match key {
        K::Named(Named::ArrowLeft) => Some(Key::Left),
        K::Named(Named::ArrowRight) => Some(Key::Right),
        K::Named(Named::ArrowUp) => Some(Key::Up),
        K::Named(Named::ArrowDown) => Some(Key::Down),
        K::Named(Named::Enter) => Some(Key::Enter),
        K::Named(Named::Escape) => Some(Key::Escape),
        K::Named(Named::Delete) => Some(Key::Delete),
        K::Named(Named::F2) => Some(Key::F2),
        _ => None,
    }
}

/// The folder watcher, on a thread of its own: it polls
/// `scan::fingerprint` every `scan::POLL` and says `Rescan` when it moves.
fn folder() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut sender| {
            std::thread::spawn(move || {
                use iced::futures::{executor::block_on, SinkExt};
                let mut last = None;
                loop {
                    // Resolved each round so a `user-dirs.dirs` written after
                    // login is picked up.
                    let now = scan::desktop_dir().map(|d| scan::fingerprint(&d));
                    if now != last {
                        last = now;
                        if block_on(sender.send(Message::Rescan)).is_err() {
                            return;
                        }
                    }
                    std::thread::sleep(scan::POLL);
                }
            });
        })
    })
}

/// The control-socket watcher, on a thread of its own with a connection of
/// its own. A fresh connection is a full re-read: the daemon may have started
/// before the compositor, or outlived one.
fn compositor() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut sender| {
            std::thread::spawn(move || {
                use iced::futures::{executor::block_on, SinkExt};
                let mut send = |m: Message| block_on(sender.send(m)).is_ok();
                loop {
                    let Some(mut client) = Client::connect()
                        .ok()
                        .and_then(|mut c| c.subscribe(KINDS).ok().map(|()| c))
                    else {
                        std::thread::sleep(RETRY);
                        continue;
                    };
                    if !send(Message::Reconfigured) || !send(Message::Refresh) {
                        return;
                    }
                    while let Ok(event) = client.wait_event() {
                        let message = match event.kind {
                            EventKind::Config => Message::Reconfigured,
                            _ => Message::Refresh,
                        };
                        if !send(message) {
                            return;
                        }
                    }
                    std::thread::sleep(RETRY);
                }
            });
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn boot_opens_every_output_at_once() {
        let mut seen = HashMap::new();
        let plan = reconcile(
            &[],
            &live(&["DP-1", "HDMI-A-1"]),
            &mut seen,
            Instant::now(),
            Duration::ZERO,
        );
        assert_eq!(plan.open, live(&["DP-1", "HDMI-A-1"]));
        assert!(!plan.wait);
    }

    #[test]
    fn hotplug_settles_then_unplug_closes() {
        let mut seen = HashMap::new();
        let t0 = Instant::now();
        let a = Id::unique();
        let plan = reconcile(&[(a, "DP-1")], &live(&["DP-1", "DP-2"]), &mut seen, t0, SETTLE);
        assert!(plan.open.is_empty() && plan.wait);
        let plan = reconcile(
            &[(a, "DP-1")],
            &live(&["DP-1", "DP-2"]),
            &mut seen,
            t0 + SETTLE,
            SETTLE,
        );
        assert_eq!(plan.open, live(&["DP-2"]));
        let plan = reconcile(&[(a, "DP-1")], &live(&["DP-2"]), &mut seen, t0 + SETTLE, SETTLE);
        assert_eq!(plan.close, vec![a]);
    }

    #[test]
    fn no_answer_changes_nothing() {
        let mut seen = HashMap::new();
        let plan = reconcile(&[(Id::unique(), "DP-1")], &[], &mut seen, Instant::now(), SETTLE);
        assert_eq!(plan, Plan::default());
    }
}
