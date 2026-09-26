// SPDX-License-Identifier: AGPL-3.0-only
//! Now Playing: the active MPRIS player on the session bus (ADR 0065).
//!
//! Feeds the taskbar's `now-playing` widget. Same shape as [`crate::status`]:
//! one watcher thread, an `mpsc` feed the GUI drains with [`Handle::try_recv`],
//! and a separate [`Actions`] handle for the transport buttons, so a click is
//! never answered on a draw path.
//!
//! Event-driven, never polled: the watcher lists the `org.mpris.MediaPlayer2.*`
//! names once, then follows `NameOwnerChanged` for players coming and going
//! and `PropertiesChanged` for everything they say. Which player is shown, and
//! how its properties map, lives in the bus-free `model` submodule.
//!
//! Titles, artists, albums and art URLs are the human's media: nothing here
//! logs them. Diagnostics name the player's bus name and nothing else.

mod model;

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::OwnedValue;
use zbus::MatchRule;

use model::{ArtCache, ArtLookup, Dedupe, Players, MPRIS_PREFIX};

/// One change to what is playing.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// `None`: nothing playing, or a player sitting idle. The widget
    /// compresses to zero width rather than drawing an empty card.
    Player(Option<NowPlaying>),
}

/// The player the widget shows.
#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    /// MPRIS bus-name suffix, e.g. `spotify` for `org.mpris.MediaPlayer2.spotify`.
    pub player: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub art: Option<Art>,
    pub status: Playback,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_pause: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    Playing,
    Paused,
    Stopped,
}

/// Cover art, read from a `file://` `mpris:artUrl`. An `http(s)` URL is not
/// fetched: the service makes no network requests on a player's say-so, and
/// ships no HTTP client to make them with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Art {
    /// Stable identity for the image (the resolved URL), so the GUI can cache
    /// the decoded handle and skip re-decoding unchanged art.
    pub key: String,
    pub bytes: Arc<[u8]>,
}

#[derive(Debug)]
enum Command {
    Previous,
    PlayPause,
    Next,
}

/// The GUI's end of the watcher.
pub struct Handle {
    updates: Receiver<Update>,
    commands: Sender<Command>,
}

impl Handle {
    /// The next change, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// A handle for the transport buttons, cheap to clone into view closures.
    pub fn actions(&self) -> Actions {
        Actions {
            commands: self.commands.clone(),
        }
    }
}

/// Transport controls for the active player. Every call returns at once; the
/// watcher thread does the bus round trip.
#[derive(Debug, Clone)]
pub struct Actions {
    commands: Sender<Command>,
}

impl Actions {
    pub fn previous(&self) {
        let _ = self.commands.send(Command::Previous);
    }

    pub fn play_pause(&self) {
        let _ = self.commands.send(Command::PlayPause);
    }

    pub fn next(&self) {
        let _ = self.commands.send(Command::Next);
    }
}

/// Start watching. The thread lives until the [`Handle`] and every
/// [`Actions`] cloned from it are dropped.
pub fn spawn() -> Handle {
    spawn_with(session)
}

fn spawn_with<C>(connect: C) -> Handle
where
    C: Fn() -> zbus::Result<Connection> + Send + 'static,
{
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("eclipse-media".into())
        .spawn(move || run(connect, updates_tx, commands_rx));
    Handle { updates, commands }
}

const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// How long to wait before reconnecting after the session bus hangs up.
const RETRY: Duration = Duration::from_secs(5);

/// A player that does not answer within this is skipped, so one hung process
/// cannot freeze the feed or queue the human's clicks behind it.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

fn session() -> zbus::Result<Connection> {
    zbus::blocking::connection::Builder::session()?
        .method_timeout(CALL_TIMEOUT)
        .build()
}

/// Debug-level diagnostics. The crate has no logger, so this is stderr in
/// debug builds and silent in release. Callers pass bus names only.
fn debug(args: std::fmt::Arguments<'_>) {
    if cfg!(debug_assertions) {
        eprintln!("eclipse-media: {args}");
    }
}

/// Everything the watcher thread waits on, in one queue.
enum Msg {
    Command(Command),
    /// The `Handle` and every `Actions` are gone.
    Quit,
    /// A signal from connection generation `.0`.
    Signal(u64, zbus::Message),
    /// Connection generation `.0` closed.
    BusGone(u64),
    /// A cover read finished.
    Art(String, Option<Arc<[u8]>>),
}

struct Service {
    tx: Sender<Msg>,
    updates: Sender<Update>,
    connection: Option<Connection>,
    generation: u64,
    players: Players,
    art: ArtCache,
    dedupe: Dedupe,
}

fn run<C>(connect: C, updates: Sender<Update>, commands: Receiver<Command>)
where
    C: Fn() -> zbus::Result<Connection>,
{
    let (tx, rx) = mpsc::channel();
    let forward = tx.clone();
    let forwarded = std::thread::Builder::new()
        .name("eclipse-media-actions".into())
        .spawn(move || {
            for command in commands {
                if forward.send(Msg::Command(command)).is_err() {
                    return;
                }
            }
            let _ = forward.send(Msg::Quit);
        });
    if forwarded.is_err() {
        return;
    }

    let mut service = Service {
        tx,
        updates,
        connection: None,
        generation: 0,
        players: Players::default(),
        art: ArtCache::default(),
        dedupe: Dedupe::default(),
    };
    let mut retry_at = Instant::now();
    loop {
        if service.connection.is_none() && Instant::now() >= retry_at {
            if let Err(error) = connect().and_then(|c| service.attach(c)) {
                debug(format_args!("session bus unavailable: {error}"));
                retry_at = Instant::now() + RETRY;
            }
        }
        let msg = if service.connection.is_some() {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(retry_at.saturating_duration_since(Instant::now()))
        };
        match msg {
            Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
            Ok(Msg::Command(command)) => service.command(command),
            Ok(Msg::Signal(generation, message)) if generation == service.generation => {
                service.signal(&message);
            }
            Ok(Msg::BusGone(generation)) if generation == service.generation => {
                debug(format_args!("session bus closed; reconnecting"));
                service.connection = None;
                service.players.clear();
                service.publish();
                retry_at = Instant::now() + RETRY;
            }
            Ok(Msg::Signal(..) | Msg::BusGone(_)) => {} // A previous connection's.
            Ok(Msg::Art(url, bytes)) => {
                service.art.insert(url, bytes);
                service.publish();
            }
        }
    }
}

impl Service {
    /// Subscribe first, then list, so a player that appears in between is
    /// heard rather than missed.
    fn attach(&mut self, connection: Connection) -> zbus::Result<()> {
        self.generation += 1;
        let owners = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(DBUS)?
            .interface(DBUS)?
            .member("NameOwnerChanged")?
            .arg0ns("org.mpris.MediaPlayer2")?
            .build();
        let properties = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .path(MPRIS_PATH)?
            .interface(PROPERTIES)?
            .member("PropertiesChanged")?
            .arg(0, PLAYER)?
            .build();
        self.pump(owners, &connection)?;
        self.pump(properties, &connection)?;

        let names: Vec<String> = connection
            .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "ListNames", &())?
            .body()
            .deserialize()?;
        self.players.clear();
        for name in names.iter().filter(|n| n.starts_with(MPRIS_PREFIX)) {
            let owner = connection
                .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "GetNameOwner", &(name,))
                .and_then(|reply| reply.body().deserialize::<String>());
            if let Ok(owner) = owner {
                self.players.upsert(name, &owner);
                refresh(&connection, &mut self.players, name);
            }
        }
        self.connection = Some(connection);
        self.publish();
        Ok(())
    }

    /// Forward every message matching `rule` on a thread that makes no calls,
    /// so the socket reader is never parked behind a full match queue (the
    /// same split as `status::pump`).
    fn pump(&self, rule: MatchRule<'static>, connection: &Connection) -> zbus::Result<()> {
        let signals = MessageIterator::for_match_rule(rule, connection, Some(64))?;
        let tx = self.tx.clone();
        let generation = self.generation;
        std::thread::Builder::new()
            .name("eclipse-media-signals".into())
            .spawn(move || {
                for message in signals {
                    let Ok(message) = message else { break };
                    if tx.send(Msg::Signal(generation, message)).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Msg::BusGone(generation));
            })
            .map_err(|error| zbus::Error::Failure(error.to_string()))?;
        Ok(())
    }

    fn signal(&mut self, message: &zbus::Message) {
        let Some(connection) = self.connection.clone() else {
            return;
        };
        let header = message.header();
        match header.member().map(|m| m.as_str()) {
            Some("NameOwnerChanged") => {
                let Ok((name, _old, new)) = message.body().deserialize::<(String, String, String)>() else {
                    return;
                };
                if !name.starts_with(MPRIS_PREFIX) {
                    return;
                }
                if new.is_empty() {
                    self.players.remove(&name);
                } else {
                    self.players.upsert(&name, &new);
                    refresh(&connection, &mut self.players, &name);
                }
            }
            Some("PropertiesChanged") => {
                let Some(sender) = header.sender().map(|s| s.to_string()) else {
                    return;
                };
                let Ok((interface, changed, invalidated)) =
                    message
                        .body()
                        .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
                else {
                    return;
                };
                if interface != PLAYER {
                    return;
                }
                for name in self.players.names_owned_by(&sender) {
                    self.players.apply(&name, &changed);
                    if !invalidated.is_empty() {
                        refresh(&connection, &mut self.players, &name);
                    }
                }
            }
            _ => return,
        }
        self.publish();
    }

    fn command(&mut self, command: Command) {
        let (Some(connection), Some(name)) = (&self.connection, self.players.active()) else {
            return;
        };
        let method = match command {
            Command::Previous => "Previous",
            Command::PlayPause => "PlayPause",
            Command::Next => "Next",
        };
        if let Err(error) = connection.call_method(Some(name), MPRIS_PATH, Some(PLAYER), method, &()) {
            // The error text is the player's; it stays out of the log.
            let kind = match error {
                zbus::Error::MethodError(..) => "refused",
                zbus::Error::InputOutput(_) => "timed out or hung up",
                _ => "failed",
            };
            debug(format_args!("{name}: {method} {kind}"));
        }
    }

    /// Send the widget what it should show now, if that changed. A cover not
    /// read yet is fetched on its own thread; the widget gets the track now
    /// and the art when it lands.
    fn publish(&mut self) {
        let tx = &self.tx;
        let art_cache = &mut self.art;
        let shown = self.players.choose().map(|(name, player)| {
            let art = match player.meta.art_url.as_deref() {
                None => None,
                Some(url) => match art_cache.lookup(url) {
                    ArtLookup::Ready(art) => Some(art),
                    ArtLookup::Read => {
                        let url = url.to_owned();
                        let tx = tx.clone();
                        let spawned = std::thread::Builder::new()
                            .name("eclipse-media-art".into())
                            .spawn(move || {
                                let bytes = model::read_art(&url);
                                let _ = tx.send(Msg::Art(url, bytes));
                            });
                        if spawned.is_err() {
                            debug(format_args!("{name}: cannot start the art reader"));
                        }
                        None
                    }
                    ArtLookup::None | ArtLookup::Pending => None,
                },
            };
            model::now_playing(name, player, art)
        });
        if let Some(update) = self.dedupe.admit(shown) {
            let _ = self.updates.send(update);
        }
    }
}

/// Re-read every player property. A player that does not answer keeps what it
/// last said.
fn refresh(connection: &Connection, players: &mut Players, name: &str) {
    let reply = connection
        .call_method(Some(name), MPRIS_PATH, Some(PROPERTIES), "GetAll", &(PLAYER,))
        .and_then(|reply| reply.body().deserialize::<HashMap<String, OwnedValue>>());
    match reply {
        Ok(props) => players.apply(name, &props),
        Err(_) => debug(format_args!("{name}: GetAll failed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No bus: the feed stays quiet, the buttons are harmless, and dropping
    /// the handle ends the thread. Never touches the human's session bus.
    #[test]
    fn without_a_bus_the_feed_is_quiet_and_actions_are_harmless() {
        let h = spawn_with(|| Err(zbus::Error::Failure("no bus in tests".into())));
        let a = h.actions();
        a.clone().play_pause();
        a.previous();
        a.next();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(h.try_recv(), None);
    }
}
