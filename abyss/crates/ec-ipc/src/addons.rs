// SPDX-License-Identifier: AGPL-3.0-only
//! Installed add-ons, as `get_config` reports them (ADR 0066).
//!
//! Every `get_config` reply carries `addons: [{id, name, hooks, capture_requested}]`
//! and `hooks_on: [..]`. Read-only: an add-on is installed by its package, and
//! nothing on the socket turns one on. `capture_requested` is only a request;
//! the grant is a `capture` line in `policy.kdl`.

use serde_json::{json, Value};

use crate::{Client, Result};

/// One installed add-on manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addon {
    pub id: String,
    pub name: String,
    /// Every hook the manifest names, including other hosts' (`activity-lens`).
    pub hooks: Vec<String>,
    pub capture_requested: bool,
}

/// The add-on half of a `get_config` reply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Addons {
    pub addons: Vec<Addon>,
    /// abyss hooks that are on, e.g. `"taskbar-widgets"`.
    pub hooks_on: Vec<String>,
}

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

impl Addon {
    /// Parse one `addons` entry. `None` if it is not that shape.
    pub fn from_json(v: &Value) -> Option<Addon> {
        Some(Addon {
            id: v.get("id")?.as_str()?.to_owned(),
            name: v.get("name")?.as_str()?.to_owned(),
            hooks: strs(v.get("hooks")),
            capture_requested: v.get("capture_requested")?.as_bool()?,
        })
    }
}

impl Addons {
    /// Whether an abyss hook is on.
    pub fn is_on(&self, hook: &str) -> bool {
        self.hooks_on.iter().any(|h| h == hook)
    }
}

/// `addons` and `hooks_on` out of a `get_config` reply. Missing fields (an
/// older compositor) read as "no add-ons"; malformed entries are skipped.
pub fn addons_from_config(reply: &Value) -> Addons {
    Addons {
        addons: reply
            .get("addons")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Addon::from_json).collect())
            .unwrap_or_default(),
        hooks_on: strs(reply.get("hooks_on")),
    }
}

impl Client {
    /// Installed add-ons and the hooks they turn on.
    pub fn addons(&mut self) -> Result<Addons> {
        let reply = self.call("get_config", json!({}))?;
        Ok(addons_from_config(&reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_get_config_fields() {
        let reply = json!({
            "keys": [],
            "addons": [
                {"id": "oracle-eyes", "name": "Oracle Eyes",
                 "hooks": ["annotations", "region-select"], "capture_requested": true},
                {"id": "broken"},
            ],
            "hooks_on": ["annotations", "region-select"],
        });
        let a = addons_from_config(&reply);
        assert_eq!(
            a.addons,
            [Addon {
                id: "oracle-eyes".into(),
                name: "Oracle Eyes".into(),
                hooks: vec!["annotations".into(), "region-select".into()],
                capture_requested: true,
            }]
        );
        assert!(a.is_on("annotations"));
        assert!(!a.is_on("taskbar-widgets"));
        assert_eq!(addons_from_config(&json!({"keys": []})), Addons::default());
    }
}
