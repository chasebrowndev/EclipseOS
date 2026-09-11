// SPDX-License-Identifier: AGPL-3.0-only
//! The control socket, as much of it as a bar needs.
//!
//! The bar makes three read calls and five action calls, all blocking and all
//! short. A dropped socket clears the client so the next interaction
//! reconnects rather than failing forever — the bar must not need a restart
//! because the compositor did.

use eclipse_ipc::{Client, Error, EventKind};
use serde_json::{json, Value};

use crate::model::{parse_focused, parse_windows, parse_workspaces, Snapshot};

/// The events that change anything the bar draws.
pub const KINDS: &[EventKind] = &[
    EventKind::Window,
    EventKind::Workspace,
    EventKind::Focus,
    EventKind::Output,
    EventKind::ConfigError,
];

#[derive(Default)]
pub struct Conn {
    client: Option<Client>,
}

impl Conn {
    pub fn new() -> Self {
        Conn { client: None }
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    /// Attach to the socket if we are not already. Cheap to call on every
    /// pass: it is a no-op once connected.
    pub fn ensure(&mut self) {
        if self.client.is_some() {
            return;
        }
        if let Ok(client) = Client::connect() {
            self.client = Some(client);
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Option<Value> {
        let client = self.client.as_mut()?;
        match client.call(method, params) {
            Ok(v) => Some(v),
            Err(e) => {
                if matches!(e, Error::Connect(_) | Error::Io(_)) {
                    self.client = None;
                }
                None
            }
        }
    }

    /// One pass over the three read methods. A query that fails leaves its
    /// part of the snapshot empty rather than tearing the whole bar down.
    pub fn snapshot(&mut self) -> Snapshot {
        self.ensure();
        if self.client.is_none() {
            return Snapshot::default();
        }
        let workspaces = self
            .call("get_workspaces", json!({}))
            .map(|v| parse_workspaces(&v))
            .unwrap_or_default();
        let windows = self
            .call("get_windows", json!({}))
            .map(|v| parse_windows(&v))
            .unwrap_or_default();
        let focused = self
            .call("get_focused", json!({}))
            .and_then(|v| parse_focused(&v));
        Snapshot {
            connected: self.client.is_some(),
            workspaces,
            windows,
            focused,
            clock: crate::clock::time(),
        }
    }

    pub fn focus_window(&mut self, handle: u64) {
        self.call("focus_window", json!({ "handle": handle }));
    }

    pub fn close_window(&mut self, handle: u64) {
        self.call("close_window", json!({ "handle": handle }));
    }

    /// `workspace` is the 1-based wire index, exactly as `get_workspaces`
    /// reported it.
    pub fn switch_workspace(&mut self, workspace: usize) {
        self.call("switch_workspace", json!({ "workspace": workspace }));
    }
}
