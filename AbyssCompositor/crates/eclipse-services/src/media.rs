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
//!
//! `https` cover art is fetched by spawning `curl` (the `curl` submodule)
//! while remote art is on. The setting is given to [`spawn`], so a service
//! started with it off never fetches, not even before its first command;
//! [`Handle::set_remote_art`] changes it later. `http` art is never fetched.

mod curl;
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

/// Cover art, read from a `file://` `mpris:artUrl`, or fetched from an
/// `https://` one via `curl` while remote art is on. `http://` is never
/// fetched.
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
    SetRemoteArt(bool),
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

    /// Whether `https` cover art is fetched (`bar.widgets.now-playing.remote-art`).
    /// Starts as given to [`spawn`]. Off drops fetched covers and re-sends the current track
    /// without its remote art; on again fetches the current track's.
    pub fn set_remote_art(&self, on: bool) {
        let _ = self.commands.send(Command::SetRemoteArt(on));
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

/// Start watching, fetching `https` cover art only if `remote_art`. The
/// thread lives until the [`Handle`] and every [`Actions`] cloned from it are
/// dropped.
pub fn spawn(remote_art: bool) -> Handle {
    spawn_with(session, Arc::new(read_art), remote_art)
}

/// Reads one cover, by URL. Injectable so tests never touch the network.
type Fetch = Arc<dyn Fn(&str) -> Option<Arc<[u8]>> + Send + Sync>;

fn spawn_with<C>(connect: C, fetch: Fetch, remote_art: bool) -> Handle
where
    C: Fn() -> zbus::Result<Connection> + Send + 'static,
{
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("eclipse-media".into())
        .spawn(move || run(connect, fetch, remote_art, updates_tx, commands_rx));
    Handle { updates, commands }
}

/// The real reader: `https` through `curl`, `file` from disk. Blocking.
fn read_art(url: &str) -> Option<Arc<[u8]>> {
    if model::is_remote(url) {
        return curl::fetch(url)
            .map_err(|failure| debug(format_args!("remote art: {}", failure.word())))
            .ok();
    }
    model::read_art(url)
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
    remote_art: bool,
    fetch: Fetch,
}

fn run<C>(connect: C, fetch: Fetch, remote_art: bool, updates: Sender<Update>, commands: Receiver<Command>)
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

    let mut service = Service::new(tx, updates, fetch, remote_art);
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
            Ok(Msg::Art(url, bytes)) => service.art_landed(url, bytes),
        }
    }
}

impl Service {
    fn new(tx: Sender<Msg>, updates: Sender<Update>, fetch: Fetch, remote_art: bool) -> Self {
        Service {
            tx,
            updates,
            connection: None,
            generation: 0,
            players: Players::default(),
            art: ArtCache::default(),
            dedupe: Dedupe::default(),
            remote_art,
            fetch,
        }
    }

    /// A cover read finished. Keyed by URL, so a cover landing after the
    /// track changed is cached but never shown on the new track.
    fn art_landed(&mut self, url: String, bytes: Option<Arc<[u8]>>) {
        self.art.insert(url, bytes, self.remote_art);
        self.publish();
    }

    fn set_remote_art(&mut self, on: bool) {
        if on == self.remote_art {
            return;
        }
        self.remote_art = on;
        if !on {
            self.art.drop_remote();
        }
        // Off: the current track goes out again without remote art. On: its
        // art is looked up afresh, and fetched.
        self.publish();
    }

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
        if let Command::SetRemoteArt(on) = command {
            return self.set_remote_art(on);
        }
        let (Some(connection), Some(name)) = (&self.connection, self.players.active()) else {
            return;
        };
        let method = match command {
            Command::Previous => "Previous",
            Command::PlayPause => "PlayPause",
            Command::Next => "Next",
            Command::SetRemoteArt(_) => return,
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
        let remote = self.remote_art;
        let fetch = &self.fetch;
        let shown = self.players.choose().map(|(name, player)| {
            let art = match player.meta.art_url.as_deref() {
                None => None,
                Some(url) => match art_cache.lookup(url, remote) {
                    ArtLookup::Ready(art) => Some(art),
                    ArtLookup::Read => {
                        let url = url.to_owned();
                        let tx = tx.clone();
                        let fetch = fetch.clone();
                        let spawned = std::thread::Builder::new()
                            .name("eclipse-media-art".into())
                            .spawn(move || {
                                let bytes = fetch(&url);
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
        let h = spawn_with(
            || Err(zbus::Error::Failure("no bus in tests".into())),
            Arc::new(|_: &str| None),
            true,
        );
        h.set_remote_art(false);
        h.set_remote_art(true);
        let a = h.actions();
        a.clone().play_pause();
        a.previous();
        a.next();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(h.try_recv(), None);
    }

    use zbus::zvariant::Value;

    const NAME: &str = "org.mpris.MediaPlayer2.spotify";

    /// A service with no bus. Fetches record their URL and return `bytes`;
    /// their results queue on the returned receiver until a test delivers
    /// them, as the run loop would.
    fn service(bytes: Option<&'static [u8]>) -> (Service, Receiver<Msg>, Receiver<Update>, Receiver<String>) {
        service_with(bytes, true)
    }

    fn service_with(
        bytes: Option<&'static [u8]>,
        remote_art: bool,
    ) -> (Service, Receiver<Msg>, Receiver<Update>, Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        let (updates_tx, updates) = mpsc::channel();
        let (seen_tx, seen) = mpsc::channel();
        let seen_tx = std::sync::Mutex::new(seen_tx);
        let fetch: Fetch = Arc::new(move |url: &str| {
            let _ = seen_tx.lock().unwrap().send(url.to_owned());
            bytes.map(Arc::from)
        });
        let mut s = Service::new(tx, updates_tx, fetch, remote_art);
        s.players.upsert(NAME, ":1.9");
        (s, rx, updates, seen)
    }

    fn track(s: &mut Service, title: &str, art: &str) {
        let meta: HashMap<String, Value<'static>> = [
            ("xesam:title".to_owned(), Value::from(title.to_owned())),
            ("mpris:artUrl".to_owned(), Value::from(art.to_owned())),
        ]
        .into_iter()
        .collect();
        let props: HashMap<String, OwnedValue> = [
            ("PlaybackStatus", Value::from("Playing")),
            ("Metadata", Value::from(meta)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), OwnedValue::try_from(v).unwrap()))
        .collect();
        s.players.apply(NAME, &props);
        s.publish();
    }

    /// Deliver the next finished read to the service.
    fn land(s: &mut Service, rx: &Receiver<Msg>) -> String {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Msg::Art(url, bytes)) => {
                s.art_landed(url.clone(), bytes);
                url
            }
            _ => panic!("expected a finished art read"),
        }
    }

    fn shown(updates: &Receiver<Update>) -> Vec<(String, Option<String>)> {
        updates
            .try_iter()
            .map(|Update::Player(np)| {
                let np = np.expect("a track");
                (np.title, np.art.map(|a| a.key))
            })
            .collect()
    }

    const A: &str = "https://i.scdn.co/image/a";
    const B: &str = "https://i.scdn.co/image/b";

    #[test]
    fn https_art_is_fetched_and_http_is_refused() {
        let (mut s, rx, updates, seen) = service(Some(b"jpg"));
        track(&mut s, "One", "http://i.example/a");
        track(&mut s, "Two", A);
        assert_eq!(land(&mut s, &rx), A);
        assert_eq!(
            seen.try_iter().collect::<Vec<_>>(),
            [A],
            "http never reached the fetcher"
        );
        assert_eq!(
            shown(&updates),
            [
                ("One".into(), None),
                ("Two".into(), None),
                ("Two".into(), Some(A.into()))
            ]
        );
    }

    #[test]
    fn switching_off_clears_remote_art_and_on_refetches() {
        let (mut s, rx, updates, seen) = service(Some(b"jpg"));
        track(&mut s, "Two", A);
        land(&mut s, &rx);
        let _ = shown(&updates);
        s.command(Command::SetRemoteArt(false));
        assert_eq!(shown(&updates), [("Two".into(), None)], "re-emitted without art");
        s.command(Command::SetRemoteArt(false));
        assert!(shown(&updates).is_empty(), "no-op");
        s.command(Command::SetRemoteArt(true));
        assert_eq!(land(&mut s, &rx), A, "re-fetched");
        assert_eq!(shown(&updates), [("Two".into(), Some(A.into()))]);
        assert_eq!(seen.try_iter().count(), 2);
    }

    #[test]
    fn while_off_https_art_is_not_fetched() {
        let (mut s, rx, updates, seen) = service(Some(b"jpg"));
        s.command(Command::SetRemoteArt(false));
        track(&mut s, "Two", A);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(seen.try_iter().count(), 0);
        assert_eq!(shown(&updates), [("Two".into(), None)]);
    }

    /// Started with remote art off, the very first publish (what `attach`
    /// does on connect, before any queued command is drained) fetches
    /// nothing.
    #[test]
    fn spawned_off_never_fetches_before_its_first_command() {
        let (mut s, rx, updates, seen) = service_with(Some(b"jpg"), false);
        track(&mut s, "Two", A);
        s.publish();
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(seen.try_iter().count(), 0);
        assert_eq!(shown(&updates), [("Two".into(), None)]);
        // Turning it on is what first fetches.
        s.command(Command::SetRemoteArt(true));
        assert_eq!(land(&mut s, &rx), A);
        assert_eq!(shown(&updates), [("Two".into(), Some(A.into()))]);
    }

    #[test]
    fn a_failed_fetch_is_cached_not_retried() {
        let (mut s, rx, updates, seen) = service(None);
        track(&mut s, "Two", A);
        land(&mut s, &rx);
        track(&mut s, "Two again", A);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(seen.try_iter().count(), 1);
        assert!(shown(&updates).iter().all(|(_, art)| art.is_none()));
    }

    #[test]
    fn old_art_landing_after_a_track_change_is_not_shown() {
        let (mut s, rx, updates, _seen) = service(Some(b"jpg"));
        track(&mut s, "A", A);
        track(&mut s, "B", B);
        assert_eq!(shown(&updates), [("A".into(), None), ("B".into(), None)]);
        // Both reads are in flight; deliver A's while B is shown. The reader
        // threads may finish in either order, so pick A's out explicitly.
        let mut results: Vec<(String, Option<Arc<[u8]>>)> = (0..2)
            .map(|_| match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Msg::Art(url, bytes)) => (url, bytes),
                _ => panic!("expected a finished art read"),
            })
            .collect();
        results.sort_by(|x, y| x.0.cmp(&y.0));
        let [(url_a, bytes_a), (url_b, bytes_b)]: [_; 2] = results.try_into().unwrap();
        s.art_landed(url_a, bytes_a);
        assert!(shown(&updates).is_empty(), "A's art never shown on B");
        s.art_landed(url_b, bytes_b);
        assert_eq!(shown(&updates), [("B".into(), Some(B.into()))]);
    }

    #[test]
    fn a_fetch_in_flight_across_a_toggle_is_not_doubled_or_shown_while_off() {
        let (mut s, rx, updates, seen) = service(Some(b"jpg"));
        track(&mut s, "Two", A);
        s.command(Command::SetRemoteArt(false));
        s.command(Command::SetRemoteArt(true));
        assert_eq!(land(&mut s, &rx), A);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "one fetch");
        assert_eq!(seen.try_iter().count(), 1);
        assert_eq!(shown(&updates).last(), Some(&("Two".into(), Some(A.into()))));

        // Off while in flight: the result is dropped, not shown or cached.
        let (mut s, rx, updates, _) = service(Some(b"jpg"));
        track(&mut s, "Two", A);
        s.command(Command::SetRemoteArt(false));
        land(&mut s, &rx);
        assert!(shown(&updates).iter().all(|(_, art)| art.is_none()));
    }
}
