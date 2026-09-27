// SPDX-License-Identifier: AGPL-3.0-only
//! The control socket, as much of it as a bar needs.
//!
//! The bar makes three read calls and five action calls, all blocking and all
//! short. A dropped socket clears the client so the next interaction
//! reconnects rather than failing forever — the bar must not need a restart
//! because the compositor did.

use eclipse_ipc::{Client, Error, EventKind};
use eclipse_ui::tokens::popup::Anchor;
use serde_json::{json, Value};

use crate::model::{parse_focused, parse_windows, parse_workspaces, Snapshot};
use crate::widgets;

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
    /// `bar.clock.hour-12`: `11:15 PM` rather than `23:15`.
    pub hour_12: bool,
    /// `bar.clock.date-mdy`: `9/12/26` rather than ISO `2026-09-12`.
    pub date_mdy: bool,
    /// `bar.popup-anchor`: where every popup the bar owns grows from — the
    /// context menu and the tray drawers alike, one setting for all of them.
    pub popup_anchor: Anchor,
    /// `bar.eye`: let Oracle-Eyes' beacon open the eclipse into an eye.
    pub eye: bool,
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
            hour_12: eclipse_ui::tokens::clock::HOUR_12,
            date_mdy: eclipse_ui::tokens::clock::DATE_MDY,
            popup_anchor: eclipse_ui::tokens::popup::ANCHOR,
            eye: true,
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
    /// id of the focused one. The list decides which outputs have a bar
    /// (`app::reconcile`) and resolves each bar's connector name to its id.
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

    /// The scalar `bar.*` keys. A missing key keeps its default rather than
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
                Some("bar.clock.hour-12") => {
                    if let Some(b) = value.and_then(Value::as_bool) {
                        cfg.hour_12 = b;
                    }
                }
                Some("bar.clock.date-mdy") => {
                    if let Some(b) = value.and_then(Value::as_bool) {
                        cfg.date_mdy = b;
                    }
                }
                Some("bar.eye") => {
                    if let Some(b) = value.and_then(Value::as_bool) {
                        cfg.eye = b;
                    }
                }
                Some("bar.popup-anchor") => match value.and_then(Value::as_str) {
                    Some("cell") => cfg.popup_anchor = Anchor::Cell,
                    Some("pointer") => cfg.popup_anchor = Anchor::Pointer,
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

    /// `bar.widgets.*`, `bar.motion.*` and the `widget` blocks (ADR 0065).
    /// Fail-soft like the others: no reply is the defaults.
    pub fn widgets_config(&mut self) -> widgets::Config {
        self.ensure();
        match self.call("get_config", json!({ "schema": false })) {
            Some(v) => parse_widgets(&v),
            None => widgets::Config::default(),
        }
    }

    pub fn switch_workspace(&mut self, workspace: usize) {
        self.call("switch_workspace", json!({ "workspace": workspace }));
    }

    /// A live compositor-config radius, fail-soft the same way as
    /// [`bar_config`]/[`tray_config`]: `None` on any failure, so the caller
    /// keeps whatever value it already had rather than resetting to a
    /// hardcoded token on a transient drop.
    ///
    /// [`bar_config`]: Conn::bar_config
    /// [`tray_config`]: Conn::tray_config
    pub fn glass_radius(&mut self, path: &str) -> Option<f32> {
        self.ensure();
        let client = self.client.as_mut()?;
        eclipse_ui::ipc::fetch_config_radius(client, path)
    }
}

/// `bar.widgets.*`, `bar.motion.*` and `collections.widget` out of a
/// `get_config` reply. A key that is missing or the wrong shape keeps its
/// default; an unknown widget id is dropped (the compositor's validator has
/// already refused it, so this only matters for a newer compositor).
pub fn parse_widgets(reply: &Value) -> widgets::Config {
    use std::time::Duration;

    use eclipse_ipc::widgets::{widgets_from_config, WidgetKind};
    use eclipse_services::custom::{Kind, WidgetSpec};
    use eclipse_ui::motion::Curve;
    use widgets::WidgetId;

    let mut cfg = widgets::Config::default();
    let ids = |v: &Value| -> Option<Vec<WidgetId>> {
        v.as_array().map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter_map(WidgetId::parse)
                .collect()
        })
    };
    for key in reply.get("keys").and_then(Value::as_array).into_iter().flatten() {
        let Some(value) = key.get("value") else {
            continue;
        };
        let (b, n, s) = (value.as_bool(), value.as_u64(), value.as_str());
        match key.get("path").and_then(Value::as_str) {
            Some("bar.widgets.order") => cfg.order = ids(value).unwrap_or(cfg.order),
            Some("bar.widgets.important") => cfg.important = ids(value).unwrap_or(cfg.important),
            Some("bar.widgets.now-playing.art") => cfg.now_playing.art = b.unwrap_or(cfg.now_playing.art),
            Some("bar.widgets.now-playing.visualizer") => {
                cfg.now_playing.visualizer = b.unwrap_or(cfg.now_playing.visualizer)
            }
            Some("bar.widgets.now-playing.remote-art") => {
                cfg.now_playing.remote_art = b.unwrap_or(cfg.now_playing.remote_art)
            }
            Some("bar.widgets.system-usage.interval-ms") => {
                cfg.usage.interval_ms = n.unwrap_or(cfg.usage.interval_ms)
            }
            Some("bar.widgets.system-usage.gpu") => cfg.usage.gpu = b.unwrap_or(cfg.usage.gpu),
            Some("bar.widgets.system-usage.disk") => cfg.usage.disk = b.unwrap_or(cfg.usage.disk),
            Some("bar.widgets.system-usage.disk-path") => {
                if let Some(p) = s {
                    cfg.usage.disk_path = p.into();
                }
            }
            Some("bar.widgets.volume.step") => {
                cfg.volume.step = n.map_or(cfg.volume.step, |n| n.min(100) as u32)
            }
            Some("bar.widgets.volume.scroll") => cfg.volume.scroll = b.unwrap_or(cfg.volume.scroll),
            Some("bar.widgets.volume.max-percent") => {
                cfg.volume.max_percent = n.map_or(cfg.volume.max_percent, |n| n.min(150) as u32)
            }
            Some("bar.motion.enabled") => cfg.motion.enabled = b.unwrap_or(cfg.motion.enabled),
            Some("bar.motion.duration-ms") => {
                if let Some(ms) = n {
                    cfg.motion.duration = Duration::from_millis(ms);
                }
            }
            Some("bar.motion.curve") => {
                if let Some(c) = s.and_then(Curve::parse) {
                    cfg.motion.curve = c;
                }
            }
            _ => {}
        }
    }
    cfg.custom = widgets_from_config(reply)
        .into_iter()
        .map(|w| WidgetSpec {
            name: w.name,
            kind: match w.kind {
                WidgetKind::Exec { argv, interval_ms } => Kind::Exec {
                    argv,
                    interval: Duration::from_millis(interval_ms.into()),
                },
                WidgetKind::Stream { argv } => Kind::Stream { argv },
                WidgetKind::Source { source, format } => Kind::Source { source, format },
            },
            icon: w.icon,
            on_click: w.on_click,
            on_scroll_up: w.on_scroll_up,
            on_scroll_down: w.on_scroll_down,
        })
        .collect();
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::WidgetId;
    use eclipse_services::custom::Kind;

    #[test]
    fn widget_keys_and_blocks_parse_and_bad_ones_keep_defaults() {
        let reply = json!({
            "keys": [
                {"path": "bar.widgets.order", "value": ["clock", "custom:weather", "sparkles"]},
                {"path": "bar.widgets.important", "value": 7},
                {"path": "bar.widgets.volume.max-percent", "value": 140},
                {"path": "bar.widgets.now-playing.visualizer", "value": false},
                {"path": "bar.widgets.now-playing.remote-art", "value": false},
                {"path": "bar.motion.duration-ms", "value": 300},
                {"path": "bar.motion.curve", "value": "linear"},
                {"path": "bar.motion.enabled", "value": "yes"},
            ],
            "collections": {"widget": [
                {"name": "weather", "kind": "exec", "exec": ["curl", "-s", "wttr.in"],
                 "interval-ms": 600000, "source": null, "format": null, "icon": null,
                 "on-click": null, "on-scroll-up": null, "on-scroll-down": null},
                {"name": "cpu", "kind": "source", "exec": null, "interval-ms": null,
                 "source": "usage.cpu", "format": "{}%", "icon": null,
                 "on-click": null, "on-scroll-up": null, "on-scroll-down": null},
            ]},
        });
        let cfg = parse_widgets(&reply);
        let d = widgets::Config::default();
        assert_eq!(
            cfg.order,
            vec![WidgetId::Clock, WidgetId::Custom("weather".into())]
        );
        assert_eq!(cfg.important, d.important);
        assert_eq!(cfg.volume.max_percent, 140);
        assert!(!cfg.now_playing.visualizer);
        assert!(!cfg.now_playing.remote_art);
        assert!(d.now_playing.remote_art, "remote art is on by default");
        assert_eq!(cfg.motion.duration.as_millis(), 300);
        assert_eq!(cfg.motion.curve.as_str(), "linear");
        assert_eq!(cfg.motion.enabled, d.motion.enabled);
        assert_eq!(cfg.custom.len(), 2);
        assert!(matches!(&cfg.custom[0].kind, Kind::Exec { interval, .. } if interval.as_secs() == 600));
        assert!(matches!(&cfg.custom[1].kind, Kind::Source { format, .. } if format == "{}%"));
    }

    #[test]
    fn no_reply_shape_is_the_defaults() {
        assert_eq!(parse_widgets(&json!(null)), widgets::Config::default());
    }
}
