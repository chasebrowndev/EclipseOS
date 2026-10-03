// SPDX-License-Identifier: AGPL-3.0-only
//! One fail-soft read of `misc.terminal-command` at startup (TERM-01), plus
//! one of `decoration.rounding` (BLUR-06), the `launcher.*` keys and
//! `ui.show-key-hints` (COMP-13) alongside it.
//!
//! An unset key, a socket nothing is listening on, and a denied query all
//! collapse to the same answer: no terminal command, so `Terminal=true`
//! entries stay hidden. That is the same "empty when nothing is there"
//! behavior every other pane in this repo gives its control-socket read.
//!
//! The launcher is spawned fresh by every keybind press and never stays open
//! long enough to have a live-reconfigure channel, so `fetch_glass` (the
//! radius and the blur mode) is read once here too, at the same startup
//! moment as the terminal command; a `decoration` change reaches it on the
//! next launch, not the current one.

use serde_json::json;

/// `None` on any failure — the launcher does not distinguish "unset" from
/// "couldn't ask", since both mean the same thing to `apps::scan`/`launch`.
pub fn fetch_terminal_command() -> Option<String> {
    let mut client = ec_ipc::Client::connect().ok()?;
    let reply = client
        .call("get_config", json!({ "path": "misc.terminal-command" }))
        .ok()?;
    reply
        .get("keys")?
        .as_array()?
        .first()?
        .get("value")?
        .as_str()
        .map(str::to_owned)
}

/// `decoration.rounding` and whether blur is on; each `None` on any failure,
/// and the caller keeps its compile-time default.
pub fn fetch_glass() -> (Option<f32>, Option<bool>) {
    ec_ui::ipc::fetch_glass()
}

/// Where the centred launcher sits on the output (`launcher.centered.anchor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    #[default]
    Center,
    Top,
    Bottom,
}

/// The `launcher.*` keys the centred launcher reads at spawn (COMP-13). Like
/// the glass, they are read once: a change reaches the next launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LauncherConfig {
    /// `launcher.centered.width`, logical px.
    pub width: u32,
    /// `launcher.centered.max-rows`.
    pub rows: usize,
    /// `launcher.centered.anchor`.
    pub anchor: Anchor,
    /// `launcher.search.*`.
    pub search: ec_services::apps::Search,
    /// `launcher.style` is `"menu"`.
    pub menu_style: bool,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        LauncherConfig {
            width: crate::WIDTH,
            rows: crate::MAX_ROWS,
            anchor: Anchor::Center,
            search: ec_services::apps::Search::default(),
            menu_style: false,
        }
    }
}

/// Every `launcher.*` key in one `get_config`. Fail-soft: no socket, a
/// refusal, or a key an older compositor does not have keeps that field's
/// default. An older compositor's `bar.launcher-style` still answers for
/// `launcher.style`.
pub fn fetch_launcher_config() -> LauncherConfig {
    let mut cfg = LauncherConfig::default();
    let Ok(mut client) = ec_ipc::Client::connect() else {
        return cfg;
    };
    let Ok(reply) = client.call("get_config", json!({})) else {
        return cfg;
    };
    let Some(keys) = reply.get("keys").and_then(|k| k.as_array()) else {
        return cfg;
    };
    let mut style_seen = false;
    for key in keys {
        let (Some(path), Some(value)) = (key.get("path").and_then(|p| p.as_str()), key.get("value")) else {
            continue;
        };
        match path {
            "launcher.style" => {
                style_seen = true;
                cfg.menu_style = value.as_str() == Some("menu");
            }
            "bar.launcher-style" if !style_seen => cfg.menu_style = value.as_str() == Some("menu"),
            "launcher.centered.width" => {
                if let Some(v) = value.as_u64() {
                    cfg.width = v as u32;
                }
            }
            "launcher.centered.max-rows" => {
                if let Some(v) = value.as_u64() {
                    cfg.rows = v as usize;
                }
            }
            "launcher.centered.anchor" => {
                cfg.anchor = match value.as_str() {
                    Some("top") => Anchor::Top,
                    Some("bottom") => Anchor::Bottom,
                    _ => Anchor::Center,
                }
            }
            "launcher.search.path-binaries" => {
                if let Some(b) = value.as_bool() {
                    cfg.search.path_binaries = b;
                }
            }
            "launcher.search.terminal-apps" => {
                if let Some(b) = value.as_bool() {
                    cfg.search.terminal_apps = b;
                }
            }
            "launcher.search.match-descriptions" => {
                if let Some(b) = value.as_bool() {
                    cfg.search.match_descriptions = b;
                }
            }
            _ => {}
        }
    }
    cfg
}

/// `launcher.style` is `"menu"`: the taskbar's start menu is the launcher
/// on this desktop, so the keybind that spawns this binary should open that
/// menu rather than a second, centred one. `false` on any failure.
pub fn menu_style() -> bool {
    fetch_launcher_config().menu_style
}

/// `ui.show-key-hints`: draw the hint line at the foot. `true` on any
/// failure — the hints are the default.
pub fn show_key_hints() -> bool {
    ec_ui::ipc::fetch_show_key_hints().unwrap_or(true)
}

/// Ask the compositor to open the bar's menu on the focused output. `true`
/// only when a bar was listening to take it (`delivered`); anything else —
/// no socket, a refusal, no bar running — is `false`, and the caller falls
/// back to drawing the centred launcher itself.
pub fn open_bar_menu() -> bool {
    let Ok(mut client) = ec_ipc::Client::connect() else {
        return false;
    };
    client
        .call("open_launcher", json!({}))
        .ok()
        .and_then(|reply| reply.get("delivered")?.as_bool())
        .unwrap_or(false)
}
