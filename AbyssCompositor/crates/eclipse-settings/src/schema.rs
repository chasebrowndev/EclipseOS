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

/// How an enum value reads on its pill. The wire spelling is a config token
/// (`cell`), right for `abyss.kdl` and wrong for a person, so the few that do
/// not read as themselves get a display form here. Keyed on the value, not on
/// a key path: this is not a key list, and a value with no entry — every value
/// the compositor grows later — reads as itself.
pub fn value_label(value: &str) -> &str {
    match value {
        // `bar.popup-anchor`: the chip is what the user clicked, so "cell"
        // is named after it. `pointer` reads the same way for
        // `floating-placement`, which shares the spelling.
        "cell" => "below chip",
        "pointer" => "at pointer",
        // `general.layout`: radiant is the default, dwindle the classic
        // tiler it replaced. The wire still carries the bare token.
        "radiant" => "Radiant (Default)",
        "dwindle" => "Dwindle Classic",
        "master" => "Master",
        other => other,
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

    /// The last segment of the path, used as the row label — or, for the few
    /// keys whose last segment names a thing rather than a setting, a
    /// display form. `bar.eye` alone reads as a body part; what the switch
    /// governs is the indicator.
    pub fn label(&self) -> &str {
        match self.path.as_str() {
            "bar.eye" => "Eye indicator",
            path => path.rsplit('.').next().unwrap_or(path),
        }
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
    fn a_label_is_the_last_segment_unless_it_names_a_thing() {
        let row = |path: &str| {
            Row::parse(&json!({
                "path": path, "file": "abyss", "type": "bool", "value": true,
            }))
            .expect("a bool row parses")
        };
        assert_eq!(row("bar.fold-when-idle").label(), "fold-when-idle");
        assert_eq!(row("bar.eye").label(), "Eye indicator");
    }

    #[test]
    fn enum_values_read_as_words_or_as_themselves() {
        assert_eq!(value_label("cell"), "below chip");
        assert_eq!(value_label("pointer"), "at pointer");
        assert_eq!(value_label("ease-out"), "ease-out");
        assert_eq!(value_label("radiant"), "Radiant (Default)");
        assert_eq!(value_label("dwindle"), "Dwindle Classic");
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
