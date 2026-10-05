// SPDX-License-Identifier: AGPL-3.0-only
//! The control socket, as much of it as a bar needs.
//!
//! The bar makes three read calls and five action calls, all blocking and all
//! short. A dropped socket clears the client so the next interaction
//! reconnects rather than failing forever — the bar must not need a restart
//! because the compositor did.

use ec_ipc::{Client, Error, EventKind};
use ec_ui::tokens::popup::Anchor;
use serde_json::{json, Value};

use crate::model::{parse_focused, parse_mode, parse_windows, parse_workspaces, Mode, Snapshot};
use crate::widgets;

/// The events that change anything the bar draws.
pub const KINDS: &[EventKind] = &[
    EventKind::Window,
    EventKind::Workspace,
    EventKind::Focus,
    EventKind::Output,
    EventKind::ConfigError,
    EventKind::Config,
    EventKind::Launcher,
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
    /// `launcher.style` (an older compositor's `bar.launcher-style`): what
    /// the eclipse button opens.
    pub launcher: LauncherStyle,
}

/// The `launcher.*` keys the start menu reads (COMP-13), at startup and on
/// every `config` event. Defaults match the schema's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuConfig {
    /// `launcher.menu.max-rows`.
    pub rows: usize,
    /// `launcher.search.*`: what the menu indexes and matches.
    pub search: ec_services::apps::Search,
}

impl Default for MenuConfig {
    fn default() -> Self {
        MenuConfig {
            rows: 8,
            search: ec_services::apps::Search::default(),
        }
    }
}

/// `launcher.style`: the separate centred launcher, or the start menu
/// the bar grows out of its own eclipse cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LauncherStyle {
    #[default]
    Centered,
    Menu,
}

/// Where each tray entry lives. `pinned: None` is "unset" — the taskbar's own
/// built-in order — which is not the same thing as an empty list, which pins
/// nothing and sends everything to the overflow drawer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayConfig {
    pub pinned: Option<Vec<String>>,
    pub hidden: Vec<String>,
    /// `bar.pinned-apps` (ADR 0074): desktop-entry ids in bar order. Rides
    /// with the tray lists because it is read from the same reply and
    /// reloaded on the same event.
    pub pinned_apps: Vec<String>,
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

/// The edge the bar's layer surface anchors to. `bar.position` is live
/// (COMP-13 §1.4): a reload that changes it re-asks every bar's anchor.
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
            hour_12: ec_ui::tokens::clock::HOUR_12,
            date_mdy: ec_ui::tokens::clock::DATE_MDY,
            popup_anchor: ec_ui::tokens::popup::ANCHOR,
            eye: true,
            launcher: LauncherStyle::Centered,
        }
    }
}

/// A chip's rectangle in its output's logical, output-local pixels, rounded
/// to whole pixels so a sub-pixel wobble in the layout is not a change.
pub type ChipBox = (i32, i32, i32, i32);

/// What the compositor was last told about each window's taskbar chip
/// (`set_window_chip_rect`), so only a change goes on the wire.
#[derive(Debug, Default)]
pub struct ChipReports {
    sent: std::collections::HashMap<u64, (String, ChipBox)>,
}

/// One call to make: set `Some` rect, or clear with `None`.
#[derive(Debug, PartialEq, Eq)]
pub struct ChipUpdate {
    pub handle: u64,
    pub output: String,
    pub rect: Option<ChipBox>,
}

impl ChipReports {
    /// What has to be sent for the chips now drawn (`now`: window handle,
    /// output, rect), and records it as sent. A window that was reported and
    /// is no longer drawn gets a clear on the output it was reported for.
    pub fn diff(&mut self, now: &[(u64, &str, ChipBox)]) -> Vec<ChipUpdate> {
        let mut out = Vec::new();
        for &(handle, output, rect) in now {
            let same = self
                .sent
                .get(&handle)
                .is_some_and(|(o, r)| o == output && *r == rect);
            if !same {
                self.sent.insert(handle, (output.to_owned(), rect));
                out.push(ChipUpdate {
                    handle,
                    output: output.to_owned(),
                    rect: Some(rect),
                });
            }
        }
        let gone: Vec<u64> = self
            .sent
            .keys()
            .filter(|h| !now.iter().any(|(n, _, _)| n == *h))
            .copied()
            .collect();
        for handle in gone {
            if let Some((output, _)) = self.sent.remove(&handle) {
                out.push(ChipUpdate {
                    handle,
                    output,
                    rect: None,
                });
            }
        }
        out
    }

    /// The call failed: send it again next time.
    pub fn forget(&mut self, handle: u64) {
        self.sent.remove(&handle);
    }

    /// The compositor went away and came back with no memory of any of it.
    pub fn reset(&mut self) {
        self.sent.clear();
    }
}

/// An output's logical height from a `get_outputs` row: the mode's height over
/// the scale, swapped for a quarter turn.
pub fn logical_height(row: &Value) -> Option<f32> {
    let mode = row.get("mode")?;
    let (w, h) = (mode.get("width")?.as_f64()?, mode.get("height")?.as_f64()?);
    let scale = row
        .get("scale")
        .and_then(Value::as_f64)
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);
    let turned = matches!(
        row.get("transform").and_then(Value::as_str),
        Some("90" | "270" | "flipped-90" | "flipped-270")
    );
    Some(((if turned { w } else { h }) / scale) as f32)
}

#[derive(Default)]
pub struct Conn {
    client: Option<Client>,
    chips: ChipReports,
}

impl Conn {
    pub fn new() -> Self {
        Conn {
            client: None,
            chips: ChipReports::default(),
        }
    }

    /// The logical height of the output named `name`, if the compositor says.
    pub fn output_height(&mut self, name: &str) -> Option<f32> {
        self.ensure();
        let v = self.call("get_outputs", json!({}))?;
        v.as_array()?
            .iter()
            .find(|r| r.get("name").and_then(Value::as_str) == Some(name))
            .and_then(logical_height)
    }

    /// Tell abyss where each window's chip is (`set_window_chip_rect`): only
    /// what changed since the last report, and a clear for each chip that is
    /// gone. Render-only on the compositor's side. A failed call is retried
    /// on the next report.
    pub fn report_chips(&mut self, now: &[(u64, &str, ChipBox)]) {
        if self.client.is_none() {
            return;
        }
        for u in self.chips.diff(now) {
            let rect = match u.rect {
                Some((x, y, w, h)) => json!({"x": x, "y": y, "w": w, "h": h}),
                None => Value::Null,
            };
            let ok = self.call(
                "set_window_chip_rect",
                json!({"id": u.handle, "output": u.output, "rect": rect}),
            );
            // A clear for a window that has since died is refused; that is
            // fine, there is nothing left to clear.
            if ok.is_none() && u.rect.is_some() {
                self.chips.forget(u.handle);
            }
        }
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
                    self.chips.reset();
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

    /// Write one string-list key. The compositor does its own capability
    /// check and validation; the bar keeps no copy, the `config` event that
    /// follows a successful write is what moves the chips.
    pub fn set_config_list(&mut self, path: &str, list: &[String]) {
        self.call("set_config_value", json!({ "path": path, "value": list }));
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
                // `bar.launcher-style` is what an older compositor calls it;
                // a current one lists only `launcher.style`.
                Some("launcher.style" | "bar.launcher-style") => match value.and_then(Value::as_str) {
                    Some("menu") => cfg.launcher = LauncherStyle::Menu,
                    Some("centered") => cfg.launcher = LauncherStyle::Centered,
                    _ => {}
                },
                _ => {}
            }
        }
        cfg
    }

    /// The start menu's `launcher.*` keys and `ui.show-key-hints`, from one
    /// `get_config`. Fail-soft: no reply, or a key an older compositor does
    /// not have, keeps the default — and the hints default to shown.
    pub fn menu_config(&mut self) -> (MenuConfig, bool) {
        let mut cfg = MenuConfig::default();
        let mut hints = true;
        self.ensure();
        let Some(v) = self.call("get_config", json!({ "schema": false })) else {
            return (cfg, hints);
        };
        let Some(keys) = v.get("keys").and_then(Value::as_array) else {
            return (cfg, hints);
        };
        for key in keys {
            let value = key.get("value");
            let flag = value.and_then(Value::as_bool);
            match key.get("path").and_then(Value::as_str) {
                Some("launcher.menu.max-rows") => {
                    if let Some(r) = value.and_then(Value::as_u64) {
                        cfg.rows = r as usize;
                    }
                }
                Some("launcher.search.path-binaries") => {
                    if let Some(b) = flag {
                        cfg.search.path_binaries = b;
                    }
                }
                Some("launcher.search.terminal-apps") => {
                    if let Some(b) = flag {
                        cfg.search.terminal_apps = b;
                    }
                }
                Some("launcher.search.match-descriptions") => {
                    if let Some(b) = flag {
                        cfg.search.match_descriptions = b;
                    }
                }
                Some("launcher.search.frecency") => {
                    if let Some(b) = flag {
                        cfg.search.frecency = b;
                    }
                }
                Some("ui.show-key-hints") => {
                    if let Some(b) = flag {
                        hints = b;
                    }
                }
                _ => {}
            }
        }
        (cfg, hints)
    }

    /// The interaction `mode` (ADR 0062). Fail-soft to `hybrid`, the schema
    /// default, so a compositor that is absent or too old leaves the bar as
    /// it was.
    pub fn mode(&mut self) -> Mode {
        self.ensure();
        self.call("get_config", json!({ "path": "mode" }))
            .map(|v| parse_mode(&v))
            .unwrap_or_default()
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
                Some("bar.pinned-apps") => cfg.pinned_apps = value.and_then(strings).unwrap_or_default(),
                _ => {}
            }
        }
        cfg
    }

    /// `bar.widgets.*`, the `widget` blocks (ADR 0065) and the `animations`
    /// events the bar plays.
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
        ec_ui::ipc::fetch_config_radius(client, path)
    }

    /// `annotations.accent`: the gold Oracle-Eyes is drawn in, which the
    /// live eye wears too. `None` when nothing answers or the key is not
    /// there, so the caller keeps [`color::ACCENT`](ec_ui::tokens::color::ACCENT).
    pub fn eye_accent(&mut self) -> Option<iced::Color> {
        self.ensure();
        let client = self.client.as_mut()?;
        ec_ui::ipc::fetch_config_color(client, "annotations.accent")
    }

    /// The air around the pill (`general.gaps-out`, and the effective
    /// `general.gaps-out-vertical` — unset reads `null` and mirrors the
    /// horizontal key, as the compositor resolves it), and the effective
    /// `general.gaps-in-vertical` below it. `None` when nothing
    /// answers, so the caller keeps what it had.
    pub fn air(&mut self) -> Option<crate::app::Air> {
        self.ensure();
        let client = self.client.as_mut()?;
        let x = ec_ui::ipc::fetch_config_radius(client, "general.gaps-out")?;
        let y = ec_ui::ipc::fetch_config_radius(client, "general.gaps-out-vertical").unwrap_or(x);
        let gi = ec_ui::ipc::fetch_config_radius(client, "general.gaps-in").unwrap_or(y);
        let inner = ec_ui::ipc::fetch_config_radius(client, "general.gaps-in-vertical").unwrap_or(gi);
        Some(crate::app::Air {
            x: x.round() as i32,
            y: y.round() as i32,
            inner: inner.round() as i32,
        })
    }

    /// Whether the compositor blurs under the bar (`decoration.blur.mode`
    /// is not `"off"`). `None` when nothing answers.
    pub fn blur(&mut self) -> Option<bool> {
        self.ensure();
        let client = self.client.as_mut()?;
        ec_ui::ipc::fetch_blur(client)
    }

    /// `misc.terminal-command` (TERM-01), read when the start menu opens:
    /// with none set, `Terminal=true` entries are left out of the list, as
    /// in `ec-launcher`. `None` on any failure.
    pub fn terminal_command(&mut self) -> Option<String> {
        self.ensure();
        let reply = self.call("get_config", json!({ "path": "misc.terminal-command" }))?;
        reply
            .get("keys")?
            .as_array()?
            .first()?
            .get("value")?
            .as_str()
            .map(str::to_owned)
    }
}

/// `bar.widgets.*`, `collections.widget` and `animations` out of a
/// `get_config` reply. A key that is missing or the wrong shape keeps its
/// default; an unknown widget id is dropped (the compositor's validator has
/// already refused it, so this only matters for a newer compositor).
pub fn parse_widgets(reply: &Value) -> widgets::Config {
    use std::time::Duration;

    use ec_ipc::widgets::{widgets_from_config, WidgetKind};
    use ec_services::custom::{Kind, WidgetSpec};
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
            _ => {}
        }
    }
    let anims = crate::motion::Anims::from_reply(reply);
    cfg.motion = anims.layout.motion;
    cfg.chip_add = anims.chip_add;
    cfg.chip_remove = anims.chip_remove;
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
    use ec_services::custom::Kind;

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
            ],
            "animations": {"events": {
                "bar-layout": {"style": "glide", "duration-ms": 300, "curve": "linear"},
                "chip-add": {"style": "fade", "duration-ms": 100, "curve": "ease-out"},
            }},
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
        assert!(cfg.motion.enabled);
        assert_eq!(cfg.chip_add.style, crate::motion::Style::Fade);
        assert_eq!(
            cfg.chip_remove, d.chip_remove,
            "an event the reply lacks keeps its default"
        );
        assert_eq!(cfg.custom.len(), 2);
        assert!(matches!(&cfg.custom[0].kind, Kind::Exec { interval, .. } if interval.as_secs() == 600));
        assert!(matches!(&cfg.custom[1].kind, Kind::Source { format, .. } if format == "{}%"));
    }

    #[test]
    fn no_reply_shape_is_the_defaults() {
        assert_eq!(parse_widgets(&json!(null)), widgets::Config::default());
    }

    #[test]
    fn chip_reports_send_only_changes_and_clear_the_gone() {
        let mut r = ChipReports::default();
        let a = (1, "DP-1", (10, 4, 100, 34));
        let b = (2, "DP-1", (120, 4, 100, 34));
        let first = r.diff(&[a, b]);
        assert_eq!(first.len(), 2);
        assert!(r.diff(&[a, b]).is_empty(), "unchanged sends nothing");

        let moved = (2, "DP-1", (130, 4, 100, 34));
        assert_eq!(
            r.diff(&[a, moved]),
            vec![ChipUpdate {
                handle: 2,
                output: "DP-1".into(),
                rect: Some((130, 4, 100, 34)),
            }]
        );
        assert_eq!(
            r.diff(&[a]),
            vec![ChipUpdate {
                handle: 2,
                output: "DP-1".into(),
                rect: None,
            }]
        );
        assert!(r.diff(&[a]).is_empty(), "a clear is sent once");

        r.forget(1);
        assert_eq!(r.diff(&[a]).len(), 1, "a failed send is retried");
        r.reset();
        assert_eq!(r.diff(&[a]).len(), 1, "a new compositor knows nothing");
    }

    #[test]
    fn logical_height_follows_scale_and_turn() {
        let row = json!({"mode": {"width": 3840, "height": 2160}, "scale": 2.0, "transform": "normal"});
        assert_eq!(logical_height(&row), Some(1080.0));
        let turned = json!({"mode": {"width": 1920, "height": 1080}, "scale": 1.0, "transform": "90"});
        assert_eq!(logical_height(&turned), Some(1920.0));
        assert_eq!(logical_height(&json!({"mode": null})), None);
    }
}
