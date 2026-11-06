// SPDX-License-Identifier: AGPL-3.0-only
//! The D-Bus half: `org.freedesktop.Notifications` at
//! `/org/freedesktop/Notifications`, on its own thread.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::thread;

use zbus::blocking::connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

use super::history::{now_secs, wire_list, Shared, WireEntry, HISTORY_BUS_NAME, HISTORY_PATH};
use super::{parse_actions, CloseReason, Command, Event, Notification, Notifications, Urgency};

const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

/// What we tell clients we can do. Deliberately short: every capability listed
/// here is one this server actually honours, because a client that believes
/// `actions` and gets none renders a notification the human cannot answer.
const CAPABILITIES: &[&str] = &["body", "body-markup", "actions", "icon-static", "persistence"];

/// The object the bus talks to. Owns nothing the GUI owns — it forwards.
struct Server {
    events: Sender<Event>,
    /// Ids are handed out from here. Zero is reserved by the spec as "not a
    /// notification", so the first one out is 1.
    last_id: u32,
    store: Shared,
}

/// Tell history watchers the list (or a live flag) changed.
async fn history_changed(conn: &zbus::Connection) {
    if let Ok(emitter) = SignalEmitter::new(conn, HISTORY_PATH) {
        let _ = HistoryServer::changed(&emitter).await;
    }
}

/// `org.eclipse.NotificationHistory`: the taskbar's window onto the store.
/// Private to the session; see `history.rs` for what is and is not kept.
/// Served under its own bus name, not `org.freedesktop.Notifications`: a
/// sandbox granted the latter to post its own notices must not get a read of
/// everyone's bodies or a way to press other apps' buttons.
struct HistoryServer {
    store: Shared,
    /// Back to the toasts GUI, so an invoked toast leaves the screen.
    events: Sender<Event>,
}

/// The connection owns two names and zbus routes by path, so a call addressed
/// to `org.freedesktop.Notifications` would reach this object too. Refuse
/// anything not sent to the history's own name.
fn addressed_to_history(header: &zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
    match header.destination() {
        Some(d) if d.as_str() == HISTORY_BUS_NAME => Ok(()),
        _ => Err(zbus::fdo::Error::AccessDenied(
            "notification history is served on org.eclipse.NotificationHistory".into(),
        )),
    }
}

#[interface(name = "org.eclipse.NotificationHistory")]
impl HistoryServer {
    /// Newest first.
    fn list(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> zbus::fdo::Result<Vec<WireEntry>> {
        addressed_to_history(&header)?;
        Ok(wire_list(&self.store.lock()))
    }

    async fn remove(
        &self,
        id: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        addressed_to_history(&header)?;
        let changed = self.store.lock().remove(id);
        if changed {
            let _ = Self::changed(&emitter).await;
        }
        Ok(())
    }

    async fn clear(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        addressed_to_history(&header)?;
        let changed = self.store.lock().clear();
        if changed {
            let _ = Self::changed(&emitter).await;
        }
        Ok(())
    }

    /// Press a button on a notification that is still live. False (nothing
    /// sent) once it has expired, been dismissed or withdrawn.
    async fn invoke_action(
        &self,
        id: u32,
        key: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<bool> {
        addressed_to_history(&header)?;
        if !self.store.lock().invoke(id, &key) {
            return Ok(false);
        }
        if let Ok(e) = SignalEmitter::new(conn, OBJECT_PATH) {
            let _ = Server::action_invoked(&e, id, &key).await;
            let _ = Server::notification_closed(&e, id, CloseReason::Dismissed as u32).await;
        }
        // The toast, if drawn, goes; the GUI does not echo a close back.
        let _ = self.events.send(Event::Closed {
            id,
            reason: CloseReason::Dismissed,
        });
        let _ = Self::changed(&emitter).await;
        Ok(true)
    }

    #[zbus(signal)]
    async fn changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[interface(name = "org.freedesktop.Notifications")]
impl Server {
    /// The spec's one interesting method. Everything the hint bag carries that
    /// we do not render is dropped here rather than passed on, so the GUI sees
    /// a plain struct and no `Value`s.
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &mut self,
        #[zbus(connection)] conn: &zbus::Connection,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let urgency = hints
            .get("urgency")
            .and_then(|v| v.downcast_ref::<u8>().ok())
            .map_or(Urgency::Normal, Urgency::from_hint);
        let transient = hints
            .get("transient")
            .and_then(|v| v.downcast_ref::<bool>().ok())
            .unwrap_or(false);
        // `image-path` wins over the positional icon when both are given —
        // that is the order the spec's own note gives them in.
        let icon = hints
            .get("image-path")
            .and_then(|v| v.downcast_ref::<&str>().ok())
            .map_or(app_icon, str::to_owned);

        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.last_id = self.last_id.wrapping_add(1).max(1);
            self.last_id
        };

        let notification = Notification {
            id,
            app_name,
            summary,
            body,
            icon,
            urgency,
            actions: parse_actions(&actions),
            expires_in: Notification::lifetime(expire_timeout, urgency),
            transient,
        };

        // A full channel or a gone GUI is not an error the client can act on;
        // it still gets its id back.
        let stored = self.store.lock().posted(&notification, now_secs());
        let _ = self.events.send(Event::Posted(Box::new(notification)));
        if stored {
            history_changed(conn).await;
        }
        id
    }

    /// The sending application withdrawing its own notification.
    async fn close_notification(&mut self, id: u32, #[zbus(connection)] conn: &zbus::Connection) {
        let _ = self.events.send(Event::Closed {
            id,
            reason: CloseReason::ByCall,
        });
        if self.store.lock().closed(id) {
            history_changed(conn).await;
        }
    }

    fn get_capabilities(&self) -> Vec<&'static str> {
        CAPABILITIES.to_vec()
    }

    /// name, vendor, version, spec version.
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        (
            "eclipse-notifications",
            "EclipseOS",
            env!("CARGO_PKG_VERSION"),
            "1.2",
        )
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

/// Take the well-known name and serve until the returned handle is dropped.
///
/// Fails if something else already owns `org.freedesktop.Notifications` — we
/// never steal it. One notification daemon per session is the whole contract,
/// and taking it from a running one silently breaks the desktop we are on.
pub fn spawn() -> zbus::Result<Notifications> {
    let (event_tx, event_rx) = mpsc::channel();
    let (command_tx, command_rx) = mpsc::channel();
    let compositor_tx = event_tx.clone();
    let history = Shared::default();
    let history_for_serve = history.clone();

    // The connection is built on the service thread so that the whole D-Bus
    // side lives and dies there, but the caller still learns about a name
    // clash synchronously — hence the handshake channel.
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("eclipse-notifications".into())
        .spawn(move || {
            // Not `Builder::name()`: it requests the name but throws the reply
            // away, so an already-owned name comes back as a happily-built
            // connection that owns nothing. Request it ourselves and insist on
            // being the primary owner. `DoNotQueue` keeps us from lurking in the
            // queue, and we never ask for `ReplaceExisting` — stealing the name
            // from a running daemon is the failure we are guarding against.
            let connection = connection::Builder::session()
                .and_then(|b| {
                    b.serve_at(
                        OBJECT_PATH,
                        Server {
                            events: event_tx.clone(),
                            last_id: 0,
                            store: history.clone(),
                        },
                    )
                })
                .and_then(|b| {
                    b.serve_at(
                        HISTORY_PATH,
                        HistoryServer {
                            store: history,
                            events: event_tx,
                        },
                    )
                })
                .and_then(connection::Builder::build)
                .and_then(|connection| {
                    for name in [BUS_NAME, HISTORY_BUS_NAME] {
                        match connection.request_name_with_flags(name, RequestNameFlags::DoNotQueue.into())? {
                            RequestNameReply::PrimaryOwner => {}
                            _ => return Err(zbus::Error::NameTaken),
                        }
                    }
                    Ok(connection)
                });

            let connection = match connection {
                Ok(connection) => {
                    if ready_tx.send(Ok(())).is_err() {
                        return;
                    }
                    connection
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };

            serve(&connection, &command_rx, &history_for_serve);
        })
        .map_err(|e| zbus::Error::InputOutput(std::sync::Arc::new(e)))?;

    match ready_rx.recv() {
        Ok(Ok(())) => {
            // Only the daemon that owns the name shows the compositor's config
            // problems (ADR 0064); a second instance would double them.
            super::compositor::spawn(compositor_tx);
            Ok(Notifications::new(event_rx, command_tx))
        }
        Ok(Err(e)) => Err(e),
        // The thread died before it said anything; treat it as a failure to
        // take the name rather than hanging the caller.
        Err(_) => Err(zbus::Error::NameTaken),
    }
}

/// Emit what the human did, until the GUI drops its handle. Method calls are
/// dispatched by the connection's own task, so this thread only carries the
/// outbound half.
fn serve(connection: &zbus::blocking::Connection, commands: &mpsc::Receiver<Command>, history: &Shared) {
    let object_server = connection.object_server();
    let Ok(iface) = object_server.interface::<_, Server>(OBJECT_PATH) else {
        return;
    };
    let emitter = iface.signal_emitter();

    while let Ok(command) = commands.recv() {
        // Whatever the GUI did, that toast is no longer live.
        let id = match &command {
            Command::Close { id, .. } | Command::Invoke { id, .. } => *id,
        };
        let history_changed_now = history.lock().closed(id);
        let result = match command {
            Command::Close { id, reason } => {
                zbus::block_on(Server::notification_closed(emitter, id, reason as u32))
            }
            Command::Invoke { id, key } => zbus::block_on(async {
                Server::action_invoked(emitter, id, &key).await?;
                // The spec pairs the two: a clicked notification is a closed
                // notification unless the client asked for `resident`, which
                // we do not advertise.
                Server::notification_closed(emitter, id, CloseReason::Dismissed as u32).await
            }),
        };
        if history_changed_now {
            zbus::block_on(async {
                if let Ok(e) = SignalEmitter::new(connection.inner(), HISTORY_PATH) {
                    let _ = HistoryServer::changed(&e).await;
                }
            });
        }
        if result.is_err() {
            // The bus is gone. Nothing further will succeed either.
            return;
        }
    }
}
