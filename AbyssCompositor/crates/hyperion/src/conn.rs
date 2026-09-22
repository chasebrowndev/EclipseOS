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
    /// Fold after `idle_seconds` without input. Independent of
    /// `fold_when_inactive`: either one folds, both may hold at once.
    pub fold_when_idle: bool,
    pub idle_seconds: u32,
    /// Slide duration for height and exclusive zone together; zero snaps.
    pub fold_duration_ms: u32,
    pub fold_curve: FoldCurve,
    pub position: BarPosition,
}

/// Where each tray entry lives. `pinned: None` is "unset" — the taskbar's own
/// built-in order — which is not the same thing as an empty list, which pins
/// nothing and sends everything to the overflow drawer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayConfig {
    pub pinned: Option<Vec<String>>,
    pub hidden: Vec<String>,
}

/// `bar.fold-curve`, kept as an enum so `BarConfig` stays `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldCurve {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl FoldCurve {
    /// Ease `t` in `0.0..=1.0`. Matches the compositor's `ANIMATION_CURVES`
    /// vocabulary; the shapes are the usual quadratics.
    pub fn ease(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            FoldCurve::Linear => t,
            FoldCurve::EaseIn => t * t,
            FoldCurve::EaseOut => t * (2.0 - t),
            FoldCurve::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    -1.0 + (4.0 - 2.0 * t) * t
                }
            }
        }
    }
}

/// The edge the bar's layer surface anchors to. `bar.position` is
/// `reload: restart` (COMP-13 §1.4) — the layer surface's anchor is set once
/// in `main::bar` before the window exists, so this field is read at startup
/// only and is not updated by `Config` events the way the fold keys are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarPosition {
    Top,
    Bottom,
}

impl Default for BarConfig {
    fn default() -> Self {
        BarConfig {
            fold_when_inactive: false,
            fold_height: 4,
            fold_when_idle: false,
            idle_seconds: 30,
            fold_duration_ms: 150,
            fold_curve: FoldCurve::EaseOut,
            position: BarPosition::Top,
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
                Some("bar.fold-when-idle") => {
                    if let Some(b) = value.and_then(Value::as_bool) {
                        cfg.fold_when_idle = b;
                    }
                }
                Some("bar.idle-seconds") => {
                    if let Some(s) = value.and_then(Value::as_u64) {
                        cfg.idle_seconds = s as u32;
                    }
                }
                Some("bar.fold-duration-ms") => {
                    if let Some(d) = value.and_then(Value::as_u64) {
                        cfg.fold_duration_ms = d as u32;
                    }
                }
                Some("bar.fold-curve") => match value.and_then(Value::as_str) {
                    Some("linear") => cfg.fold_curve = FoldCurve::Linear,
                    Some("ease-in") => cfg.fold_curve = FoldCurve::EaseIn,
                    Some("ease-out") => cfg.fold_curve = FoldCurve::EaseOut,
                    Some("ease-in-out") => cfg.fold_curve = FoldCurve::EaseInOut,
                    _ => {}
                },
                Some("bar.position") => match value.and_then(Value::as_str) {
                    Some("bottom") => cfg.position = BarPosition::Bottom,
                    Some("top") => cfg.position = BarPosition::Top,
                    _ => {}
                },
                _ => {}
            }
        }
        cfg
    }

    /// `bar.tray.pinned` and `bar.tray.hidden`. Separate from [`bar_config`]
    /// because `BarConfig` is `Copy` and these are lists; same fail-soft rule —
    /// a compositor without the keys leaves the built-in order.
    ///
    /// [`bar_config`]: Conn::bar_config
    pub fn tray_config(&mut self) -> TrayConfig {
        let mut cfg = TrayConfig::default();
        self.ensure();
        let Some(v) = self.call("get_config", json!({ "schema": false })) else {
            return cfg;
        };
        let Some(keys) = v.get("keys").and_then(Value::as_array) else {
            return cfg;
        };
        let strings = |v: &Value| -> Option<Vec<String>> {
            v.as_array()
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        };
        for key in keys {
            let value = key.get("value");
            match key.get("path").and_then(Value::as_str) {
                Some("bar.tray.pinned") => cfg.pinned = value.and_then(strings),
                Some("bar.tray.hidden") => cfg.hidden = value.and_then(strings).unwrap_or_default(),
                _ => {}
            }
        }
        cfg
    }

    pub fn switch_workspace(&mut self, workspace: usize) {
        self.call("switch_workspace", json!({ "workspace": workspace }));
    }
}
