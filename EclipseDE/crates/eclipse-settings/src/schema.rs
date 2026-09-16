// SPDX-License-Identifier: AGPL-3.0-only
//! The schema as it arrives on the wire, and the control each type maps to.
//!
//! Everything here is driven by `get_config {schema: true}` (COMP-13 §2).
//! There is deliberately no hand-written list of config keys in this crate —
//! a key the compositor grows shows up here on the next connect, and
//! `tests/coverage.rs` fails if this module has nothing to render it with.

use serde_json::Value;

/// Which file owns a key. The wire has no `owner` field; the `"file"` string
/// is the owner, and `policy` means this app may display but never write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum File {
    Abyss,
    Policy,
}

impl File {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "abyss" => Some(Self::Abyss),
            "policy" => Some(Self::Policy),
            _ => None,
        }
    }
}

/// The widget a key is edited with. Chosen from the declared type alone, so
/// the mapping is total over the schema and testable without a compositor.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    /// `bool`
    Toggle,
    /// `int` / `float` with bounds
    Slider { min: f64, max: f64, integral: bool },
    /// `enum` with few enough variants to lay out side by side
    Segmented(Vec<String>),
    /// `enum` with too many variants for pills
    Dropdown(Vec<String>),
    /// `string` / `color` — validated per keystroke, written on commit
    Text { color: bool },
    /// `string-list` — shown, not edited. `set_config_value` v1 writes one
    /// scalar at a dotted path, so a list needs the node editor that the
    /// bind/windowrule work brings. Displayed read-only rather than hidden:
    /// never hide that a setting exists.
    List,
}

/// Above this many variants an enum gets a dropdown instead of pills.
const MAX_PILLS: usize = 4;

/// The one type-to-widget mapping in the app. Keyed on the wire spelling from
/// `ty_json` (`config_rpc.rs`) so the runtime path and the coverage test agree
/// by construction instead of by a comment asking them to.
pub fn control_for(ty: &str, constraints: &Value) -> Option<Control> {
    let num = |integral: bool| {
        let min = constraints.get("min")?.as_f64()?;
        let max = constraints.get("max")?.as_f64()?;
        Some(Control::Slider { min, max, integral })
    };
    match ty {
        "bool" => Some(Control::Toggle),
        "int" => num(true),
        "float" => num(false),
        "string" => Some(Control::Text { color: false }),
        "color" => Some(Control::Text { color: true }),
        "string-list" => Some(Control::List),
        "enum" => {
            let values: Vec<String> = constraints
                .get("values")?
                .as_array()?
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<_>>()?;
            if values.is_empty() {
                None
            } else if values.len() <= MAX_PILLS {
                Some(Control::Segmented(values))
            } else {
                Some(Control::Dropdown(values))
            }
        }
        _ => None,
    }
}

/// One row of `{"keys": [...]}`.
#[derive(Debug, Clone)]
pub struct Row {
    pub path: String,
    pub file: File,
    pub value: Value,
    pub default: Value,
    pub source: Option<String>,
    pub readable: bool,
    pub writable: bool,
    pub doc: String,
    /// True when a change only takes effect after a compositor restart.
    pub needs_restart: bool,
    pub control: Control,
}

impl Row {
    /// Parse one row of a `schema: true` reply. Returns `None` for a row this
    /// build has no control for, so an older settings app talking to a newer
    /// compositor degrades to "that key is missing" rather than panicking.
    pub fn parse(v: &Value) -> Option<Self> {
        let ty = v.get("type")?.as_str()?;
        let constraints = v.get("constraints").unwrap_or(&Value::Null);
        Some(Row {
            path: v.get("path")?.as_str()?.to_owned(),
            file: File::parse(v.get("file")?.as_str()?)?,
            value: v.get("value").cloned().unwrap_or(Value::Null),
            default: v.get("default").cloned().unwrap_or(Value::Null),
            source: v.get("source").and_then(Value::as_str).map(str::to_owned),
            readable: v.get("readable").and_then(Value::as_bool).unwrap_or(false),
            writable: v.get("writable").and_then(Value::as_bool).unwrap_or(false),
            doc: v
                .get("doc")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            needs_restart: v.get("reload").and_then(Value::as_str) == Some("restart"),
            control: control_for(ty, constraints)?,
        })
    }

    /// The last segment of the path, used as the row label.
    pub fn label(&self) -> &str {
        self.path.rsplit('.').next().unwrap_or(&self.path)
    }

    /// Policy-owned keys are read-only here by construction, not by a check at
    /// the point of writing: editing them requires the policy editor.
    pub fn locked(&self) -> bool {
        self.file == File::Policy || !self.writable
    }

    /// The value as a display string.
    pub fn display(&self) -> String {
        match &self.value {
            Value::Null => "—".into(),
            Value::Bool(b) => b.to_string(),
            Value::String(s) => s.clone(),
            Value::Array(a) => {
                if a.is_empty() {
                    "—".into()
                } else {
                    a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
                }
            }
            other => other.to_string(),
        }
    }

    pub fn as_bool(&self) -> bool {
        self.value.as_bool().unwrap_or(false)
    }

    pub fn as_f64(&self) -> f64 {
        self.value.as_f64().unwrap_or(0.0)
    }
}

/// Parse a whole `get_config` reply, dropping rows this build cannot render.
pub fn parse_keys(reply: &Value) -> Vec<Row> {
    reply
        .get("keys")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(Row::parse).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn enum_width_picks_pills_or_dropdown() {
        let pills = control_for("enum", &json!({"values": ["a", "b"]}));
        assert!(matches!(pills, Some(Control::Segmented(_))));
        let many: Vec<String> = (0..MAX_PILLS + 1).map(|i| i.to_string()).collect();
        assert!(matches!(
            control_for("enum", &json!({ "values": many })),
            Some(Control::Dropdown(_))
        ));
    }

    #[test]
    fn a_number_without_bounds_has_no_slider() {
        assert_eq!(control_for("int", &Value::Null), None);
    }

    #[test]
    fn policy_rows_are_locked_even_though_they_parse() {
        let row = Row::parse(&json!({
            "path": "misc.scripted-input", "file": "policy", "value": null,
            "default": false, "source": null, "readable": false, "writable": false,
            "type": "bool", "constraints": null, "doc": "d", "reload": "live"
        }))
        .expect("policy rows are rendered, not dropped");
        assert!(row.locked());
        assert_eq!(row.control, Control::Toggle);
    }
}
