// SPDX-License-Identifier: AGPL-3.0-only
//! Bar state and the update loop.
//!
//! The model is a single [`Snapshot`]. Every event the compositor pushes
//! triggers one refetch of the three lists rather than a delta merge: the
//! events carry enough to reconstruct the state, but a refetch is one round
//! trip on a socket we already hold and keeps the model single-sourced.

use iced::{Subscription, Task};
use iced_layershell::to_layer_message;

use crate::conn::Conn;
use crate::model::Snapshot;

/// How long the event thread sleeps between passes. Short enough that the
/// clock's minute boundary is never more than this late, long enough that an
/// idle bar costs nothing.
const POLL: std::time::Duration = std::time::Duration::from_millis(500);

#[to_layer_message]
#[derive(Debug, Clone)]
pub enum Message {
    /// Something changed; re-read the compositor.
    Refresh,
    /// A workspace pill was clicked. 1-based wire index.
    Switch(usize),
    /// A window entry was clicked.
    Focus(u64),
    /// A window entry was middle-clicked.
    Close(u64),
}

pub struct App {
    pub conn: Conn,
    pub snapshot: Snapshot,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let mut conn = Conn::new();
        let snapshot = conn.snapshot();
        App { conn, snapshot }
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Refresh => {}
        Message::Switch(index) => app.conn.switch_workspace(index),
        Message::Focus(handle) => app.conn.focus_window(handle),
        Message::Close(handle) => app.conn.close_window(handle),
        // `to_layer_message` injects the layer-control variants. The bar never
        // sends one — it is anchored for its whole life — but the match must
        // still be total.
        _ => return Task::none(),
    }
    // Every branch above either changed compositor state or was told state
    // changed, so all of them end the same way.
    app.snapshot = app.conn.snapshot();
    Task::none()
}

/// Compositor events and the clock tick, on one thread.
///
/// `iced::time::every` is not in our feature set, and would not help here
/// anyway: the same thread that blocks on the socket is the one that has to
/// notice the minute roll over.
pub fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(32, async move |mut sender| {
            std::thread::spawn(move || {
                let mut client = None;
                let mut minute = String::new();
                loop {
                    if client.is_none() {
                        if let Ok(mut c) = eclipse_ipc::Client::connect() {
                            if c.subscribe(crate::conn::KINDS).is_ok() {
                                client = Some(c);
                                // A fresh connection means the bar may have
                                // been started before the compositor was.
                                if sender.try_send(Message::Refresh).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    if let Some(c) = client.as_mut() {
                        loop {
                            match c.poll_event() {
                                Ok(Some(_)) => {
                                    if sender.try_send(Message::Refresh).is_err() {
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
                    let now = crate::clock::time();
                    if now != minute {
                        minute = now;
                        if sender.try_send(Message::Refresh).is_err() {
                            return;
                        }
                    }
                    std::thread::sleep(POLL);
                }
            });
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Trust, Window, Workspace};

    fn app() -> App {
        App {
            conn: Conn::new(),
            snapshot: Snapshot::default(),
        }
    }

    #[test]
    fn an_injected_layer_variant_is_a_no_op_not_a_panic() {
        let mut a = app();
        let _ = update(&mut a, Message::SizeChange((100, 34)));
        assert_eq!(a.snapshot, Snapshot::default());
    }

    #[test]
    fn a_refresh_without_a_compositor_yields_an_empty_snapshot() {
        let mut a = app();
        a.snapshot.workspaces.push(Workspace {
            index: 1,
            output_name: "DP-1".into(),
            active: true,
            windows: 0,
        });
        let _ = update(&mut a, Message::Refresh);
        // No socket in the test environment, so the refetch must degrade to
        // "nothing connected" rather than keeping a stale list around.
        if !a.snapshot.connected {
            assert!(a.snapshot.workspaces.is_empty());
        }
    }

    #[test]
    fn the_bar_renders_a_secret_window_by_its_label_only() {
        let w = Window {
            handle: 3,
            app_id: "org.x.Vault".into(),
            title: "seed phrase".into(),
            workspace: Some(1),
            focused: true,
            trust: Trust::Secret,
        };
        assert_eq!(w.label(), "Protected window");
    }
}
