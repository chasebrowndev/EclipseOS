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
    EventKind::Config,
];

/// What the bar reads out of `bar.*` once, at startup, and again on every
/// `config` event. Defaults match the schema's own so a compositor that is
/// not listening leaves the bar in its ordinary always-open state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarConfig {
    pub fold_when_inactive: bool,
    pub fold_height: u32,
}

impl Default for BarConfig {
    fn default() -> Self {
        BarConfig {
            fold_when_inactive: false,
            fold_height: 4,
        }
    }
}

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

    /// Send a window away, or bring it back. Unlike the keybinds this names
    /// the window directly, which is what a taskbar chip needs: once a window
    /// is minimized it is by definition not the focused one.
    pub fn set_minimized(&mut self, handle: u64, minimized: bool) {
        self.call(
            "set_minimized",
            json!({ "handle": handle, "minimized": minimized }),
        );
    }

    /// `workspace` is the 1-based wire index, exactly as `get_workspaces`
    /// reported it.
    /// Every output the compositor knows about, as `(id, connector)`, plus the
    /// id of the focused one. The supervisor uses the list to decide how many
    /// bars to run; a bound bar uses it once to learn its own id from the
    /// connector name it was started with.
    pub fn outputs(&mut self) -> (Vec<(u64, String)>, Option<u64>) {
        self.ensure();
        let Some(v) = self.call("get_outputs", json!({})) else {
            return (Vec::new(), None);
        };
        let rows = v.as_array().cloned().unwrap_or_default();
        let mut focused = None;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let Some(id) = row.get("id").and_then(Value::as_u64) else {
                continue;
            };
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if row.get("focused").and_then(Value::as_bool).unwrap_or(false) {
                focused = Some(id);
            }
            out.push((id, name));
        }
        (out, focused)
    }

    /// The two `bar.*` keys. A missing key keeps its default rather than
    /// failing the fetch: an older compositor without the section must still
    /// leave the bar usable.
    pub fn bar_config(&mut self) -> BarConfig {
        let mut cfg = BarConfig::default();
        self.ensure();
        let Some(v) = self.call("get_config", json!({ "schema": false })) else {
            return cfg;
        };
        let Some(keys) = v.get("keys").and_then(Value::as_array) else {
            return cfg;
        };
        for key in keys {
            let value = key.get("value");
            match key.get("path").and_then(Value::as_str) {
                Some("bar.fold-when-inactive") => {
                    if let Some(b) = value.and_then(Value::as_bool) {
                        cfg.fold_when_inactive = b;
                    }
                }
                Some("bar.fold-height") => {
                    if let Some(h) = value.and_then(Value::as_u64) {
                        cfg.fold_height = h as u32;
                    }
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn switch_workspace(&mut self, workspace: usize) {
        self.call("switch_workspace", json!({ "workspace": workspace }));
    }
}
