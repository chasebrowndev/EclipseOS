// SPDX-License-Identifier: AGPL-3.0-only
//! The calendar drawer's events: a client of `org.eclipse.Calendar1` (ADR 0079).
//!
//! Same shape as [`crate::media`]: one watcher thread, an `mpsc` feed the GUI
//! drains with [`Handle::try_recv`], and an [`Actions`] handle for the calls
//! that go the other way. Bus round trips to the provider run on short-lived
//! threads, so a slow or hung provider never stalls the feed or the bar.
//!
//! The service follows `NameOwnerChanged` for the provider and the provider's
//! `Changed(start, end)` signal, and fetches the range the bar last asked for
//! with [`Actions::set_range`]. With no owner the bar gets
//! `Provider(false)` and an empty `Events`.
//!
//! Event data is `private` (ADR 0079): titles, locations and per-day counts
//! are never logged. Diagnostics say only that something failed.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::OwnedValue;
use zbus::MatchRule;

/// One event, as the drawer shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub title: String,
    /// Unix seconds UTC. All-day events: local midnights, `end` exclusive.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    /// Calendar id.
    pub calendar: String,
    /// `0xRRGGBB`: the event's own `colour` extra, else its calendar's.
    pub colour: Option<u32>,
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// Whether a provider owns `org.eclipse.Calendar1`.
    Provider(bool),
    /// The events overlapping `[start, end)`, replacing any earlier set.
    Events {
        start: i64,
        end: i64,
        events: Vec<Event>,
    },
}

#[derive(Debug)]
enum Command {
    SetRange(i64, i64),
    Open(String),
    OpenDay(i64),
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

    /// Waits up to `timeout` for the next change.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Update> {
        self.updates.recv_timeout(timeout).ok()
    }

    /// A handle for range changes and hand-offs, cheap to clone.
    pub fn actions(&self) -> Actions {
        Actions {
            commands: self.commands.clone(),
        }
    }
}

/// Calls toward the provider. Every one returns at once.
#[derive(Debug, Clone)]
pub struct Actions {
    commands: Sender<Command>,
}

impl Actions {
    /// The range the bar shows (visible month ±1), unix seconds UTC. Refetches.
    pub fn set_range(&self, start: i64, end: i64) {
        let _ = self.commands.send(Command::SetRange(start, end));
    }

    /// Show this event in the provider's own app.
    pub fn open(&self, id: &str) {
        let _ = self.commands.send(Command::Open(id.to_owned()));
    }

    /// Show this day (local midnight, unix seconds) in the provider's app.
    pub fn open_day(&self, day: i64) {
        let _ = self.commands.send(Command::OpenDay(day));
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
        .name("eclipse-calendar".into())
        .spawn(move || run(connect, updates_tx, commands_rx));
    Handle { updates, commands }
}

const NAME: &str = "org.eclipse.Calendar1";
const PATH: &str = "/org/eclipse/Calendar1";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";

/// How long to wait before reconnecting after the session bus hangs up.
const RETRY: Duration = Duration::from_secs(5);

/// A provider that does not answer within this is skipped.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

fn session() -> zbus::Result<Connection> {
    zbus::blocking::connection::Builder::session()?
        .method_timeout(CALL_TIMEOUT)
        .build()
}

fn debug(args: std::fmt::Arguments<'_>) {
    if cfg!(debug_assertions) {
        eprintln!("eclipse-calendar: {args}");
    }
}

enum Msg {
    Command(Command),
    Quit,
    /// A signal from connection generation `.0`.
    Signal(u64, zbus::Message),
    BusGone(u64),
    /// A fetch finished: request number, range, events (`None`: it failed).
    Fetched(u64, i64, i64, Option<Vec<Event>>),
}

struct Service {
    tx: Sender<Msg>,
    updates: Sender<Update>,
    connection: Option<Connection>,
    generation: u64,
    provider: bool,
    range: Option<(i64, i64)>,
    /// Number of the newest fetch; older replies are dropped.
    seq: u64,
}

fn run<C>(connect: C, updates: Sender<Update>, commands: Receiver<Command>)
where
    C: Fn() -> zbus::Result<Connection>,
{
    let (tx, rx) = mpsc::channel();
    let forward = tx.clone();
    let forwarded = std::thread::Builder::new()
        .name("eclipse-calendar-actions".into())
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
        provider: false,
        range: None,
        seq: 0,
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
                service.set_provider(false);
                retry_at = Instant::now() + RETRY;
            }
            Ok(Msg::Signal(..) | Msg::BusGone(_)) => {}
            Ok(Msg::Fetched(seq, start, end, events)) => service.fetched(seq, start, end, events),
        }
    }
}

impl Service {
    /// Subscribe first, then ask who owns the name, so an owner that appears
    /// in between is heard rather than missed.
    fn attach(&mut self, connection: Connection) -> zbus::Result<()> {
        self.generation += 1;
        let owners = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(DBUS)?
            .interface(DBUS)?
            .member("NameOwnerChanged")?
            .arg(0, NAME)?
            .build();
        let changed = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .path(PATH)?
            .interface(NAME)?
            .member("Changed")?
            .build();
        self.pump(owners, &connection)?;
        self.pump(changed, &connection)?;
        let owned: bool = connection
            .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "NameHasOwner", &(NAME,))?
            .body()
            .deserialize()?;
        self.connection = Some(connection);
        self.set_provider(owned);
        Ok(())
    }

    /// Forward matching messages on a thread that makes no calls.
    fn pump(&self, rule: MatchRule<'static>, connection: &Connection) -> zbus::Result<()> {
        let signals = MessageIterator::for_match_rule(rule, connection, Some(64))?;
        let tx = self.tx.clone();
        let generation = self.generation;
        std::thread::Builder::new()
            .name("eclipse-calendar-signals".into())
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

    fn set_provider(&mut self, now: bool) {
        if now == self.provider {
            // Reported once at start even when absent, so the bar can settle.
            if !now && self.seq == 0 {
                self.seq = 1;
                let _ = self.updates.send(Update::Provider(false));
            }
            return;
        }
        self.provider = now;
        self.seq += 1; // Drops any fetch in flight.
        let _ = self.updates.send(Update::Provider(now));
        if now {
            self.fetch();
        } else {
            let (start, end) = self.range.unwrap_or((0, 0));
            let _ = self.updates.send(Update::Events {
                start,
                end,
                events: Vec::new(),
            });
        }
    }

    fn signal(&mut self, message: &zbus::Message) {
        let header = message.header();
        match header.member().map(|m| m.as_str()) {
            Some("NameOwnerChanged") => {
                let Ok((name, _old, new)) = message.body().deserialize::<(String, String, String)>() else {
                    return;
                };
                if name == NAME {
                    self.set_provider(!new.is_empty());
                }
            }
            Some("Changed") => {
                let Ok((start, end)) = message.body().deserialize::<(i64, i64)>() else {
                    return;
                };
                if let Some((r_start, r_end)) = self.range {
                    if self.provider && start < r_end && end > r_start {
                        self.fetch();
                    }
                }
            }
            _ => {}
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::SetRange(start, end) => {
                self.range = Some((start, end));
                if self.provider {
                    self.fetch();
                }
            }
            Command::Open(id) => self.call("Open", move |c| {
                c.call_method(Some(NAME), PATH, Some(NAME), "Open", &(id,))
            }),
            Command::OpenDay(day) => self.call("OpenDay", move |c| {
                c.call_method(Some(NAME), PATH, Some(NAME), "OpenDay", &(day,))
            }),
        }
    }

    /// A hand-off to the provider, on its own thread.
    fn call<F>(&self, what: &'static str, f: F)
    where
        F: FnOnce(&Connection) -> zbus::Result<zbus::Message> + Send + 'static,
    {
        let (true, Some(connection)) = (self.provider, self.connection.clone()) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("eclipse-calendar-call".into())
            .spawn(move || {
                if f(&connection).is_err() {
                    debug(format_args!("{what} failed"));
                }
            });
    }

    fn fetch(&mut self) {
        let (Some(connection), Some((start, end))) = (self.connection.clone(), self.range) else {
            return;
        };
        self.seq += 1;
        let seq = self.seq;
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new()
            .name("eclipse-calendar-fetch".into())
            .spawn(move || {
                let events = fetch_events(&connection, start, end);
                let _ = tx.send(Msg::Fetched(seq, start, end, events));
            });
        if spawned.is_err() {
            debug(format_args!("cannot start a fetch"));
        }
    }

    fn fetched(&mut self, seq: u64, start: i64, end: i64, events: Option<Vec<Event>>) {
        // Stale (a newer fetch or a vanished provider), or the provider failed
        // to answer: keep what the bar has.
        if seq != self.seq || !self.provider {
            return;
        }
        if let Some(events) = events {
            let _ = self.updates.send(Update::Events { start, end, events });
        }
    }
}

type Wire = (
    String,
    String,
    i64,
    i64,
    bool,
    String,
    HashMap<String, OwnedValue>,
);

fn fetch_events(connection: &Connection, start: i64, end: i64) -> Option<Vec<Event>> {
    let colours: HashMap<String, u32> = connection
        .call_method(Some(NAME), PATH, Some(NAME), "Calendars", &())
        .ok()
        .and_then(|r| r.body().deserialize::<Vec<(String, String, u32)>>().ok())
        .map(|list| list.into_iter().map(|(id, _name, colour)| (id, colour)).collect())
        .unwrap_or_default();
    let reply = connection
        .call_method(Some(NAME), PATH, Some(NAME), "Events", &(start, end))
        .map_err(|_| debug(format_args!("Events failed")))
        .ok()?;
    let wire: Vec<Wire> = reply
        .body()
        .deserialize()
        .map_err(|_| debug(format_args!("Events reply malformed")))
        .ok()?;
    Some(
        wire.into_iter()
            .map(|(id, title, start, end, all_day, calendar, mut extras)| {
                let colour = extras
                    .remove("colour")
                    .and_then(|v| u32::try_from(v).ok())
                    .or_else(|| colours.get(&calendar).copied())
                    .map(|c| c & 0x00ff_ffff);
                let location = extras
                    .remove("location")
                    .and_then(|v| String::try_from(v).ok())
                    .filter(|l| !l.is_empty());
                Event {
                    id,
                    title,
                    start,
                    end,
                    all_day,
                    calendar,
                    colour,
                    location,
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command as Proc, Stdio};
    use std::sync::{Arc, Mutex};
    use zbus::zvariant::Value;

    struct PrivateBus(Child);
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn private_bus() -> Option<(PrivateBus, String)> {
        let mut child = Proc::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        Some((PrivateBus(child), address.trim().to_owned()))
    }

    struct Mock {
        events: Arc<Mutex<Vec<Wire>>>,
        opened: Arc<Mutex<Vec<String>>>,
    }

    #[zbus::interface(name = "org.eclipse.Calendar1")]
    impl Mock {
        fn events(&self, _start: i64, _end: i64) -> Vec<Wire> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .map(|(i, t, s, e, a, c, x)| {
                    let x = x
                        .iter()
                        .map(|(k, v)| (k.clone(), v.try_clone().unwrap()))
                        .collect();
                    (i.clone(), t.clone(), *s, *e, *a, c.clone(), x)
                })
                .collect()
        }
        fn calendars(&self) -> Vec<(String, String, u32)> {
            vec![("work".into(), "Work".into(), 0x336699)]
        }
        fn open(&self, id: String) {
            self.opened.lock().unwrap().push(id);
        }
        fn open_day(&self, _day: i64) {}
    }

    fn at(address: &str) -> zbus::blocking::connection::Builder<'static> {
        zbus::blocking::connection::Builder::address(address.to_owned().as_str()).unwrap()
    }

    fn wait(h: &Handle) -> Update {
        h.recv_timeout(Duration::from_secs(5))
            .expect("an update within 5s")
    }

    #[test]
    fn follows_a_provider_on_a_private_bus() {
        let Some((_bus, address)) = private_bus() else {
            eprintln!("skipped: no dbus-daemon on PATH");
            return;
        };
        let addr = address.clone();
        let h = spawn_with(move || at(&addr).method_timeout(CALL_TIMEOUT).build());
        let actions = h.actions();
        actions.set_range(0, 1000);
        assert_eq!(wait(&h), Update::Provider(false));

        let events = Arc::new(Mutex::new(vec![(
            "e1".to_owned(),
            "Standup".to_owned(),
            10,
            20,
            false,
            "work".to_owned(),
            HashMap::from([("location".to_owned(), Value::from("Room 4").try_into().unwrap())]),
        )]));
        let opened = Arc::new(Mutex::new(Vec::new()));
        let provider = at(&address)
            .name(NAME)
            .unwrap()
            .serve_at(
                PATH,
                Mock {
                    events: events.clone(),
                    opened: opened.clone(),
                },
            )
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(wait(&h), Update::Provider(true));
        let Update::Events {
            start,
            end,
            events: got,
        } = wait(&h)
        else {
            panic!("expected events");
        };
        assert_eq!((start, end), (0, 1000));
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].colour,
            Some(0x336699),
            "falls back to the calendar's colour"
        );
        assert_eq!(got[0].location.as_deref(), Some("Room 4"));

        // Changed inside the range refetches; outside it does not.
        events.lock().unwrap().clear();
        for range in [(5000_i64, 6000_i64), (100, 200)] {
            provider
                .emit_signal(None::<&str>, PATH, NAME, "Changed", &range)
                .unwrap();
        }
        let Update::Events { events: got, .. } = wait(&h) else {
            panic!("expected a refetch");
        };
        assert!(got.is_empty());
        assert_eq!(
            h.recv_timeout(Duration::from_millis(300)),
            None,
            "out-of-range Changed is ignored"
        );

        actions.open("e1");
        let deadline = Instant::now() + Duration::from_secs(5);
        while opened.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "Open never arrived");
            std::thread::sleep(Duration::from_millis(10));
        }

        drop(provider);
        assert_eq!(wait(&h), Update::Provider(false));
        assert_eq!(
            wait(&h),
            Update::Events {
                start: 0,
                end: 1000,
                events: Vec::new()
            }
        );
    }

    #[test]
    fn without_a_bus_the_feed_is_quiet() {
        let h = spawn_with(|| Err(zbus::Error::Failure("no bus in tests".into())));
        let a = h.actions();
        a.set_range(0, 10);
        a.open("x");
        a.open_day(0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(h.try_recv(), None);
    }
}
