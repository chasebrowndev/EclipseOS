// SPDX-License-Identifier: AGPL-3.0-only
//! The wire shapes the bar reads, and nothing else.
//!
//! The bar *lists* windows over the control socket and *acts* over it too;
//! it never gets a foreign-toplevel handle, so these structs are the whole
//! of what it knows about a window.

use serde_json::Value;

/// A window's trust class as the compositor reports it. A `secret` window's
/// title is never rendered — that is a policy invariant, not a nicety.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Private,
    Secret,
}

impl Trust {
    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("secret") => Trust::Secret,
            _ => Trust::Private,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub index: usize,
    pub output_name: String,
    pub active: bool,
    pub windows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub handle: u64,
    pub app_id: String,
    pub title: String,
    pub workspace: Option<usize>,
    pub focused: bool,
    pub trust: Trust,
}

impl Window {
    /// What the bar is allowed to draw for this window. A `secret` window is
    /// present in the list — hiding it would lie about what is running — but
    /// its client-controlled strings never reach the screen.
    pub fn label(&self) -> &str {
        if self.trust == Trust::Secret {
            return "Protected window";
        }
        if !self.title.is_empty() {
            return &self.title;
        }
        if !self.app_id.is_empty() {
            return &self.app_id;
        }
        "window"
    }
}

/// Everything one pass over the socket produced. The view is a pure function
/// of this, so a failed query degrades the bar rather than tearing it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub connected: bool,
    pub workspaces: Vec<Workspace>,
    pub windows: Vec<Window>,
    /// Handle of the focused window, from `get_focused` — authoritative over
    /// the per-window `focused` flag when the two disagree.
    pub focused: Option<u64>,
    pub clock: String,
}

fn str_at(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// `get_workspaces` replies with a bare array. Indices are 1-based on the wire
/// and stay that way here: they are what `switch_workspace` takes back.
pub fn parse_workspaces(v: &Value) -> Vec<Workspace> {
    v.as_array()
        .map(|rows| {
            rows.iter()
                .map(|w| Workspace {
                    index: w.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                    output_name: str_at(w, "output_name"),
                    active: w.get("active").and_then(Value::as_bool).unwrap_or(false),
                    windows: w.get("windows").and_then(Value::as_u64).unwrap_or(0) as usize,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `get_windows` replies with a bare array.
pub fn parse_windows(v: &Value) -> Vec<Window> {
    v.as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|w| {
                    Some(Window {
                        handle: w.get("handle").and_then(Value::as_u64)?,
                        app_id: str_at(w, "app_id"),
                        title: str_at(w, "title"),
                        workspace: w.get("workspace").and_then(Value::as_u64).map(|i| i as usize),
                        focused: w.get("focused").and_then(Value::as_bool).unwrap_or(false),
                        trust: Trust::parse(w.get("trust").and_then(Value::as_str)),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `get_focused` replies with an array of one entry per seat. The bar has one
/// cell, so it takes the first seat that has a window.
pub fn parse_focused(v: &Value) -> Option<u64> {
    v.as_array()?
        .iter()
        .find_map(|seat| seat.get("window")?.get("handle")?.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workspaces_keep_their_one_based_wire_index() {
        let ws = parse_workspaces(&json!([
            {"index": 1, "output": 3, "output_name": "DP-1", "active": true, "windows": 2, "owner": "human"},
            {"index": 2, "output": 3, "output_name": "DP-1", "active": false, "windows": 0, "owner": "human"}
        ]));
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].index, 1);
        assert!(ws[0].active);
        assert_eq!(ws[1].windows, 0);
    }

    #[test]
    fn a_secret_window_never_shows_its_title() {
        let wins = parse_windows(&json!([
            {"handle": 7, "app_id": "org.x.Vault", "title": "seed phrase",
             "floating": false, "focused": true, "trust": "secret", "no_agent": true}
        ]));
        assert_eq!(wins[0].trust, Trust::Secret);
        assert_eq!(wins[0].label(), "Protected window");
        assert!(!wins[0].label().contains("seed"));
    }

    #[test]
    fn a_private_window_falls_back_to_its_app_id() {
        let wins = parse_windows(&json!([
            {"handle": 1, "app_id": "kitty", "title": "",
             "floating": false, "focused": false, "trust": "private", "no_agent": false}
        ]));
        assert_eq!(wins[0].label(), "kitty");
    }

    #[test]
    fn a_window_without_a_handle_is_dropped_not_guessed() {
        assert!(parse_windows(&json!([{"app_id": "kitty"}])).is_empty());
    }

    #[test]
    fn focus_reads_the_first_seat_holding_a_window() {
        let v = json!([
            {"seat": "seat1", "window": null, "output": 1, "workspace": 1},
            {"seat": "seat2", "window": {"handle": 42, "app_id": "kitty", "title": "t"}}
        ]);
        assert_eq!(parse_focused(&v), Some(42));
        assert_eq!(parse_focused(&json!([])), None);
    }

    #[test]
    fn garbage_parses_to_empty_rather_than_panicking() {
        assert!(parse_workspaces(&json!({"nope": 1})).is_empty());
        assert!(parse_windows(&Value::Null).is_empty());
        assert_eq!(parse_focused(&Value::Null), None);
    }
}
