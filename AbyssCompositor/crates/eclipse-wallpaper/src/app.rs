// SPDX-License-Identifier: AGPL-3.0-only
//! The daemon: one Background-layer surface per output, kept in step with the
//! compositor's output list, and the decoded pictures they share.
//!
//! The output-following half is hyperion's (`hyperion::app::reconcile`), and
//! kept deliberately the same shape: a pure [`reconcile`] over the connector
//! names, a settle delay for a freshly hotplugged output, and an empty list
//! read as "no answer" rather than "every monitor went away".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use eclipse_ipc::{Client, EventKind};
use iced::widget::image::Handle;
use iced::window::Id;
use iced::{Size, Subscription, Task};
use iced_layershell::to_layer_message;
use serde_json::{json, Value};

use crate::config::{self, Wallpaper};

/// How long an output seen for the first time waits before it gets a surface.
/// A surface is asked for by connector name, and a hotplugged output's name
/// arrives a round trip after the output itself (see hyperion's `SETTLE`).
pub const SETTLE: Duration = Duration::from_millis(250);

/// How often the watcher retries a control socket that is not there.
const RETRY: Duration = Duration::from_secs(1);

/// What changes what the wallpaper draws.
const KINDS: &[EventKind] = &[EventKind::Output, EventKind::Config];

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    /// The output list may have moved; re-read it.
    Refresh,
    /// A settling output has waited out [`SETTLE`].
    Screens,
    /// A config reload succeeded; re-read the `wallpaper` node.
    Reconfigured,
    /// A picture finished decoding. `None` is a file that would not.
    Decoded(PathBuf, Option<Picture>),
    /// A surface's logical size, which `center` needs.
    Sized(Id, Size),
    /// The compositor took a surface away — its output went, most likely.
    Closed(Id),
    /// Nothing changed; draw again. See [`nudge`].
    Redraw,
}

/// A decoded picture, shared by every output that shows its path.
#[derive(Debug, Clone)]
pub struct Picture {
    pub handle: Handle,
    pub size: Size,
}

/// One decode, requested once per distinct path and file version.
struct Cached {
    /// The file's mtime when the decode was asked for. A reload re-decodes a
    /// path only when this moved, so replacing the file and reloading works
    /// and a reload that changed nothing costs nothing.
    mtime: Option<SystemTime>,
    /// `None` while decoding, and for good after a failure.
    picture: Option<Picture>,
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
    pub config: Wallpaper,
    images: HashMap<PathBuf, Cached>,
}

impl App {
    /// The decoded picture at `path`, if there is one yet.
    pub fn picture(&self, path: &Path) -> Option<&Picture> {
        self.images.get(path).and_then(|c| c.picture.as_ref())
    }

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
                if matches!(e, eclipse_ipc::Error::Connect(_) | eclipse_ipc::Error::Io(_)) {
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

    /// The `wallpaper` node. No reply at all is the defaults.
    fn read_config(&mut self) {
        let reply = self
            .call("get_config", json!({ "schema": false }))
            .unwrap_or(Value::Null);
        self.config = config::from_reply(&reply);
    }
}

pub fn boot() -> (App, Task<Message>) {
    let mut app = App {
        client: None,
        surfaces: HashMap::new(),
        seen: HashMap::new(),
        checking: false,
        config: Wallpaper::default(),
        images: HashMap::new(),
    };
    app.read_config();
    let task = screens(&mut app, Duration::ZERO);
    (app, task)
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Refresh => screens(app, SETTLE),
        Message::Screens => {
            app.checking = false;
            screens(app, SETTLE)
        }
        Message::Reconfigured => {
            app.read_config();
            load(app)
        }
        Message::Decoded(path, picture) => {
            let fresh = picture.is_some();
            if let Some(c) = app.images.get_mut(&path) {
                c.picture = picture;
            }
            if fresh {
                nudge()
            } else {
                Task::none()
            }
        }
        Message::Sized(id, size) => {
            let Some(s) = app.surfaces.get_mut(&id) else {
                return Task::none();
            };
            let first = s.size.is_none();
            s.size = Some(size);
            // A new surface has a renderer, and so an image cache, of its own.
            if first {
                nudge()
            } else {
                Task::none()
            }
        }
        Message::Redraw => Task::none(),
        Message::Closed(id) => {
            if app.surfaces.remove(&id).is_some() {
                return screens(app, SETTLE);
            }
            Task::none()
        }
        // The layer-shell actions `to_layer_message` added are the runtime's.
        _ => Task::none(),
    }
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

/// Bring the surfaces in line with the outputs, then make sure every picture
/// they show is decoded or on its way.
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
        tasks.push(Task::done(Message::RemoveWindow(id)));
    }
    for name in plan.open {
        tasks.push(open_surface(app, name));
    }
    if plan.wait && !app.checking {
        app.checking = true;
        tasks.push(after(SETTLE, Message::Screens));
    }
    tasks.push(load(app));
    Task::batch(tasks)
}

/// A Background-layer surface covering `name` edge to edge.
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
            // Zero on both axes with all four edges anchored: the output's size.
            size: Some((0, 0)),
            layer: Layer::Background,
            anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
            // -1: ignore every other surface's exclusive zone and cover the
            // whole output, bar strip included.
            exclusive_zone: Some(-1),
            margin: None,
            keyboard_interactivity: KeyboardInteractivity::None,
            // By name: a null output lands on whichever monitor is focused.
            output_option: OutputOption::OutputName(name),
            events_transparent: false,
            namespace: None,
        },
        id,
    })
}

/// Decode every path an open surface shows that is not decoded yet (or whose
/// file changed), and forget the ones no surface shows any more.
fn load(app: &mut App) -> Task<Message> {
    let mut wanted: Vec<PathBuf> = app
        .surfaces
        .values()
        .filter_map(|s| app.config.for_output(&s.output).path)
        .collect();
    wanted.sort();
    wanted.dedup();
    app.images.retain(|p, _| wanted.contains(p));
    let mut tasks = Vec::new();
    for path in wanted {
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if app.images.get(&path).is_some_and(|c| c.mtime == mtime) {
            continue;
        }
        app.images.insert(path.clone(), Cached { mtime, picture: None });
        tasks.push(decode(path));
    }
    Task::batch(tasks)
}

/// Decode off the UI thread: a large PNG is a noticeable fraction of a second.
fn decode(path: PathBuf) -> Task<Message> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    let p = path.clone();
    std::thread::spawn(move || {
        let _ = tx.send(read_picture(&p));
    });
    Task::future(async move { Message::Decoded(path, rx.await.ok().flatten()) })
}

/// The file at `path` as RGBA, or a warning and `None`. Never a panic: a bad
/// file is the colour alone.
pub fn read_picture(path: &Path) -> Option<Picture> {
    let decoded = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(image::ImageError::IoError)
        .and_then(|r| r.decode());
    match decoded {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (w, h) = rgba.dimensions();
            Some(Picture {
                handle: Handle::from_rgba(w, h, rgba.into_raw()),
                size: Size::new(w as f32, h as f32),
            })
        }
        Err(e) => {
            eprintln!(
                "eclipse-wallpaper: warning: {}: {e}; showing the colour only",
                path.display()
            );
            None
        }
    }
}

/// When [`nudge`] redraws, counted from the event that asked for it.
const NUDGES: [Duration; 3] = [
    Duration::from_millis(60),
    Duration::from_millis(300),
    Duration::from_millis(1200),
];

/// A few spaced redraws after a surface first meets a picture.
///
/// iced_wgpu uploads a new image on a worker thread and draws nothing for it
/// until then; the worker's "redraw now" is `window::Action::RedrawAll`,
/// which iced_layershell 0.19.1 drops (`multi_window.rs`, the `_ => {}` arm
/// of `Action::Window`). A background surface gets no input to redraw it
/// otherwise, so without this the first frame of a new picture — blank —
/// is the one that stays. Any message redraws every surface; three spaced
/// ones cover a large upload and cost nothing once it has landed.
fn nudge() -> Task<Message> {
    Task::batch(NUDGES.map(|d| after(d, Message::Redraw)))
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
        iced::window::close_events().map(Message::Closed),
        iced::event::listen_with(|event, _, id| match event {
            iced::Event::Window(
                iced::window::Event::Opened { size, .. } | iced::window::Event::Resized(size),
            ) => Some(Message::Sized(id, size)),
            _ => None,
        }),
    ])
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

    #[test]
    fn a_bad_file_is_none_not_a_panic() {
        assert!(read_picture(Path::new("/nonexistent/eclipse-wallpaper.png")).is_none());
        let dir = std::env::temp_dir().join(format!("eclipse-wallpaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp");
        let bad = dir.join("corrupt.png");
        std::fs::write(&bad, b"\x89PNG\r\n\x1a\nnot really").expect("write");
        assert!(read_picture(&bad).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
