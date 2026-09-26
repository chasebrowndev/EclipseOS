// SPDX-License-Identifier: AGPL-3.0-only
//! Typed helpers for the `widget` config collection (ADR 0065; COMP-13 §1.4).
//!
//! Read: `get_config` serves every `bar { widget "<name>" { … } }` block under
//! `collections.widget`. Write: `set_config_collection` with
//! `collection: "widget"` and one of four ops. The entry JSON is the same shape
//! both ways, so a pane can read an entry, change a field and post it back.
//!
//! Nothing here validates meaning (a known `source`, an interval in range):
//! the compositor's parser does, and its refusal comes back as an
//! [`Error::Rpc`] carrying the positioned `file:line:col: message` text.

use serde_json::{json, Map, Value};

use crate::{Client, Error, Result};

/// What a custom widget shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetKind {
    /// `exec …` run every `interval_ms` (the compositor's default is 5000).
    Exec { argv: Vec<String>, interval_ms: u32 },
    /// `exec …` with `stream #true`: runs once, one update per line.
    Stream { argv: Vec<String> },
    /// A shipped data source through a `{}` format string.
    Source { source: String, format: String },
}

/// One `widget` block, as `get_config` serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Widget {
    pub name: String,
    pub kind: WidgetKind,
    pub icon: Option<String>,
    pub on_click: Option<Vec<String>>,
    pub on_scroll_up: Option<Vec<String>>,
    pub on_scroll_down: Option<Vec<String>>,
}

/// The reply to a collection write.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteResult {
    /// The file written (or that would be, on a dry run).
    pub file: String,
    /// The entry before the write, in the `get_config` shape; `Null` if new.
    pub previous: Value,
    /// `bar.widgets.*` lists whose `custom:<name>` ids the write rewrote.
    pub references: Vec<String>,
    pub applied: bool,
    /// Dry run only: whether the result would load, and why not.
    pub valid: bool,
    pub errors: Vec<Value>,
}

fn strs(v: &Value) -> Option<Vec<String>> {
    v.as_array()?
        .iter()
        .map(|s| s.as_str().map(str::to_owned))
        .collect()
}

impl Widget {
    /// Parse one `collections.widget` entry. `None` if it is not that shape.
    pub fn from_json(v: &Value) -> Option<Widget> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let argv = |k: &str| v.get(k).and_then(strs);
        let kind = match v.get("kind")?.as_str()? {
            "exec" => WidgetKind::Exec {
                argv: argv("exec")?,
                interval_ms: v.get("interval-ms")?.as_u64()?.try_into().ok()?,
            },
            "stream" => WidgetKind::Stream { argv: argv("exec")? },
            "source" => WidgetKind::Source {
                source: s("source")?,
                format: s("format").unwrap_or_else(|| "{}".into()),
            },
            _ => return None,
        };
        Some(Widget {
            name: s("name")?,
            kind,
            icon: s("icon"),
            on_click: argv("on-click"),
            on_scroll_up: argv("on-scroll-up"),
            on_scroll_down: argv("on-scroll-down"),
        })
    }

    /// The entry in the `get_config` shape, every field present.
    pub fn to_json(&self) -> Value {
        let (kind, exec, interval, source, format) = match &self.kind {
            WidgetKind::Exec { argv, interval_ms } => {
                ("exec", json!(argv), json!(interval_ms), Value::Null, Value::Null)
            }
            WidgetKind::Stream { argv } => ("stream", json!(argv), Value::Null, Value::Null, Value::Null),
            WidgetKind::Source { source, format } => {
                ("source", Value::Null, Value::Null, json!(source), json!(format))
            }
        };
        json!({
            "name": self.name,
            "kind": kind,
            "exec": exec,
            "interval-ms": interval,
            "source": source,
            "format": format,
            "icon": self.icon,
            "on-click": self.on_click,
            "on-scroll-up": self.on_scroll_up,
            "on-scroll-down": self.on_scroll_down,
        })
    }
}

/// `collections.widget` out of a `get_config` reply. Entries that do not
/// parse are skipped rather than failing the whole list.
pub fn widgets_from_config(reply: &Value) -> Vec<Widget> {
    reply
        .pointer("/collections/widget")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Widget::from_json).collect())
        .unwrap_or_default()
}

/// A collection write, as `set_config_collection` params.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetOp<'a> {
    /// Create or replace the block; the name is `widget.name`.
    Upsert(&'a Widget),
    Remove {
        name: &'a str,
    },
    Rename {
        name: &'a str,
        new_name: &'a str,
    },
    /// Move to `index` (0-based) in the collection's order.
    Move {
        name: &'a str,
        index: usize,
    },
}

impl WidgetOp<'_> {
    pub fn params(&self, dry_run: bool) -> Value {
        let mut p = Map::new();
        p.insert("collection".into(), json!("widget"));
        let (op, name) = match self {
            WidgetOp::Upsert(w) => {
                p.insert("entry".into(), w.to_json());
                ("upsert", w.name.as_str())
            }
            WidgetOp::Remove { name } => ("remove", *name),
            WidgetOp::Rename { name, new_name } => {
                p.insert("new_name".into(), json!(new_name));
                ("rename", *name)
            }
            WidgetOp::Move { name, index } => {
                p.insert("index".into(), json!(index));
                ("move", *name)
            }
        };
        p.insert("op".into(), json!(op));
        p.insert("name".into(), json!(name));
        p.insert("dry_run".into(), json!(dry_run));
        Value::Object(p)
    }
}

impl WriteResult {
    fn from_json(v: &Value) -> Result<WriteResult> {
        let bad = || Error::Protocol(format!("unexpected set_config_collection reply: {v}"));
        Ok(WriteResult {
            file: v.get("file").and_then(Value::as_str).ok_or_else(bad)?.to_owned(),
            previous: v.get("previous").cloned().unwrap_or(Value::Null),
            references: v.get("references").and_then(strs).unwrap_or_default(),
            applied: v.get("applied").and_then(Value::as_bool).ok_or_else(bad)?,
            valid: v.get("valid").and_then(Value::as_bool).unwrap_or(true),
            errors: v
                .get("errors")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        })
    }
}

impl Client {
    /// Every custom widget, in file order.
    pub fn widgets(&mut self) -> Result<Vec<Widget>> {
        let reply = self.call("get_config", json!({"file": "abyss"}))?;
        Ok(widgets_from_config(&reply))
    }

    /// Apply one collection write. `dry_run` validates without writing.
    pub fn widget_write(&mut self, op: &WidgetOp<'_>, dry_run: bool) -> Result<WriteResult> {
        let reply = self.call("set_config_collection", op.params(dry_run))?;
        WriteResult::from_json(&reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weather() -> Widget {
        Widget {
            name: "weather".into(),
            kind: WidgetKind::Exec {
                argv: vec!["curl".into(), "-s".into(), "wttr.in".into()],
                interval_ms: 600_000,
            },
            icon: Some("weather-clear".into()),
            on_click: Some(vec!["xdg-open".into(), "https://wttr.in".into()]),
            on_scroll_up: None,
            on_scroll_down: None,
        }
    }

    #[test]
    fn entries_round_trip_the_get_config_shape() {
        let wire = json!({"name": "weather", "kind": "exec", "exec": ["curl", "-s", "wttr.in"],
            "interval-ms": 600000, "source": null, "format": null, "icon": "weather-clear",
            "on-click": ["xdg-open", "https://wttr.in"], "on-scroll-up": null, "on-scroll-down": null});
        assert_eq!(Widget::from_json(&wire), Some(weather()));
        assert_eq!(weather().to_json(), wire);
        let src = json!({"name": "cpu", "kind": "source", "exec": null, "interval-ms": null,
            "source": "usage.cpu", "format": "{}%", "icon": null, "on-click": null,
            "on-scroll-up": null, "on-scroll-down": null});
        let w = Widget::from_json(&src).unwrap();
        assert_eq!(w.to_json(), src);
        assert_eq!(
            widgets_from_config(&json!({"keys": [], "collections": {"widget": [src, {"kind": "?"}]}})),
            vec![w]
        );
    }

    #[test]
    fn ops_build_the_documented_params() {
        let w = weather();
        let p = WidgetOp::Upsert(&w).params(false);
        assert_eq!(p["collection"], "widget");
        assert_eq!(p["op"], "upsert");
        assert_eq!(p["name"], "weather");
        assert_eq!(p["entry"], w.to_json());
        assert_eq!(
            WidgetOp::Remove { name: "a" }.params(true),
            json!({"collection": "widget", "op": "remove", "name": "a", "dry_run": true})
        );
        assert_eq!(
            WidgetOp::Rename {
                name: "a",
                new_name: "b"
            }
            .params(false),
            json!({"collection": "widget", "op": "rename", "name": "a", "new_name": "b", "dry_run": false})
        );
        assert_eq!(
            WidgetOp::Move { name: "a", index: 2 }.params(false),
            json!({"collection": "widget", "op": "move", "name": "a", "index": 2, "dry_run": false})
        );
    }

    #[test]
    fn write_results_parse() {
        let r = WriteResult::from_json(&json!({"file": "/x/abyss.kdl", "previous": null,
            "references": ["bar.widgets.order"], "applied": true, "valid": true, "errors": []}))
        .unwrap();
        assert!(r.applied && r.valid);
        assert_eq!(r.references, ["bar.widgets.order"]);
        assert!(WriteResult::from_json(&json!({})).is_err());
    }
}
