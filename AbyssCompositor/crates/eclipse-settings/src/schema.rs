// SPDX-License-Identifier: AGPL-3.0-only
//! The schema as it arrives on the wire, and the control each type maps to.
//!
//! Everything here is driven by `get_config {schema: true}` (COMP-13 §2).
//! There is deliberately no hand-written list of config keys in this crate —
//! a key the compositor grows shows up here on the next connect, and
//! `tests/coverage.rs` fails if this module has nothing to render it with.

use std::borrow::Cow;

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
    /// `int` / `float` with bounds. `drag_max` is the slider's own drag span
    /// — absent for every key but the handful whose true `max` is far wider
    /// than its meaningful precision (`general.gaps-in`, `decoration.rounding`,
    /// `idle.dpms-timeout-seconds`, …), where it keeps a drag fine-grained
    /// while a typed value still reaches `max`. `None` falls back to `max`.
    Slider {
        min: f64,
        max: f64,
        integral: bool,
        drag_max: Option<f64>,
    },
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
        let drag_max = constraints.get("drag-max").and_then(Value::as_f64);
        Some(Control::Slider {
            min,
            max,
            integral,
            drag_max,
        })
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
        // `mode`: what each level adds, in the words COMP-17 §2 uses.
        "wm" => "WM",
        "hybrid" => "Hybrid",
        "de" => "Desktop",
        // Component slots: the ids are program names and read as themselves;
        // only the opt-out needs a word.
        "none" => "None",
        // `decoration.blur.mode`: what is drawn behind translucency. `glass`
        // is the refracting bevel, which people know by its product name.
        "off" => "Off",
        "blur" => "Blur",
        "frost" => "Frost",
        "glass" => "Liquid Glass",
        other => other,
    }
}

/// What an interaction mode gives you, in plain words, shown beneath the
/// `mode` control. The pills stay short; the meaning lives here.
pub fn mode_blurb(value: &str) -> Option<&'static str> {
    match value {
        "wm" => Some("Tiling with a workspaces bar only."),
        "hybrid" => Some("The bar also shows window chips, the tray and the clock."),
        "de" => Some("Hybrid, plus desktop icons and pointer-first navigation."),
        _ => None,
    }
}

/// What each blur mode draws, shown beneath `decoration.blur.mode`. Which of
/// the rows beneath apply is said by dimming the rest (`moded`), not here.
pub fn blur_blurb(value: &str) -> Option<&'static str> {
    match value {
        "off" => Some("Translucent surfaces show the desktop behind them unblurred."),
        "blur" => Some("A plain blur of whatever is behind."),
        "frost" => Some("Blur with a tint and fine grain."),
        "glass" => Some("Blur bent through a rounded bevel, with a rim light."),
        _ => None,
    }
}

/// The node whose `mode` key decides which of its other keys apply.
pub const BLUR: &str = "decoration.blur";

/// The value of a node's `mode` key that turns the node off: nothing under
/// it applies.
const MODE_OFF: &str = "off";

/// Which of a moded node's `modes` the key at `path` applies to, or `None`
/// for a key outside `node` and for the `mode` key itself.
///
/// Read from the path alone: a key in a sub-node named for a mode
/// (`decoration.blur.frost.tint`) tunes that mode only, and a key directly
/// under the node (`decoration.blur.size`) tunes every mode but off. A key the
/// compositor grows in either place is placed and dimmed with no change here.
pub fn moded<'m>(node: &str, path: &str, modes: &'m [String]) -> Option<Vec<&'m str>> {
    let rest = path.strip_prefix(node)?.strip_prefix('.')?;
    if rest == "mode" {
        return None;
    }
    let sub = rest.split_once('.').map(|(sub, _)| sub);
    Some(
        modes
            .iter()
            .map(String::as_str)
            .filter(|m| match sub {
                Some(sub) => *m == sub,
                None => *m != MODE_OFF,
            })
            .collect(),
    )
}

/// A `color` value as the wire spells it — `#rrggbb` or `#rrggbbaa` — as
/// red, green, blue and alpha bytes. Anything else is `None`: the swatch is
/// then left out, and the field beside it still says what is wrong.
pub fn rgba(value: &str) -> Option<[u8; 4]> {
    let hex = value.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if hex.len() == 8 { byte(6)? } else { 255 },
    ])
}

/// A kebab-case config token as a person reads it: hyphens become spaces and
/// the first letter is capitalised. Nothing else changes, so `gaps-in` is
/// "Gaps in" and an acronym the token already spells in capitals keeps them.
pub fn sentence_case(token: &str) -> String {
    let spaced = token.replace('-', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
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

    /// The last segment of the path in sentence case, used as the row label
    /// (`focus-follows-mouse` reads "Focus follows mouse") — or, for the few
    /// keys whose last segment names a thing rather than a setting, a
    /// display form. `bar.eye` alone reads as a body part; what the switch
    /// governs is the indicator.
    pub fn label(&self) -> Cow<'_, str> {
        Cow::Borrowed(match self.path.as_str() {
            "bar.eye" => "Eye indicator",
            "mode" => "Interaction mode",
            "components.bar" => "Bar",
            "components.launcher" => "Launcher",
            "components.notifications" => "Notifications",
            "components.control-center" => "Control center",
            // The blur group flattens its `glass` and `frost` sub-nodes
            // (`pane::group_for`), so the leaf alone would lose which mode a
            // row tunes.
            "decoration.blur.glass.refraction" => "Glass refraction",
            "decoration.blur.glass.bevel" => "Glass bevel",
            "decoration.blur.glass.dispersion" => "Glass dispersion",
            "decoration.blur.glass.rim" => "Glass rim light",
            "decoration.blur.frost.tint" => "Frost tint",
            // Tokens abbreviated for a config file, or named for a mechanism
            // rather than for what a person sees change.
            "general.col-active-border" => "Active border colour",
            "general.col-inactive-border" => "Inactive border colour",
            "general.drop-guide-color" => "Drop guide colour",
            "decoration.active-opacity" => "Active window opacity",
            "decoration.inactive-opacity" => "Inactive window opacity",
            "decoration.dim-inactive" => "Dim inactive windows",
            "decoration.glow.active" => "Glow on the active window",
            "decoration.glow.inactive" => "Glow on inactive windows",
            "input.repeat-rate" => "Key repeat rate",
            "input.repeat-delay" => "Key repeat delay",
            "input.kb-layout" => "Keyboard layout",
            "input.kb-variant" => "Keyboard variant",
            "input.kb-options" => "Keyboard options",
            "input.accel-profile" => "Pointer acceleration",
            "input.touchpad.dwt" => "Disable while typing",
            "idle.dpms-timeout-seconds" => "Screen off after (seconds)",
            "idle.lock-timeout-seconds" => "Lock after (seconds)",
            "capture.redact-app-id" => "Redact app IDs",
            // Gaps are named for where the air is, not for the mechanism;
            // the vertical pair only shows once set apart (`mirror_of`).
            "general.gaps-in" => "Between windows",
            "general.gaps-out" => "Screen edges",
            "general.gaps-in-vertical" => "Between rows",
            "general.gaps-out-vertical" => "Top and bottom",
            // A bare verb or state under its node's heading reads as an
            // instruction or a report, not as a setting.
            "xwayland.enable" => "Xwayland enabled",
            "setup.complete" => "Setup complete",
            path => return Cow::Owned(sentence_case(path.rsplit('.').next().unwrap_or(path))),
        })
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

    /// The key this one follows while unset: `general.gaps-in-vertical`
    /// mirrors `general.gaps-in` until it is given a value of its own. Read
    /// from the shape the schema gives such a key (an `-vertical` twin whose
    /// default is unset), not from a list kept here.
    pub fn mirror_of(&self) -> Option<&str> {
        self.path
            .strip_suffix("-vertical")
            .filter(|_| self.default.is_null())
    }

    /// The unit a number is in, when its doc names one. Every length in the
    /// compositor's schema is documented as "logical px".
    pub fn unit(&self) -> Option<&'static str> {
        self.doc.contains("logical px").then_some("px")
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
    fn an_unset_vertical_twin_mirrors_its_key_and_lengths_are_px() {
        let row = |path: &str, default: Value, doc: &str| {
            Row::parse(&json!({
                "path": path, "file": "abyss", "type": "int",
                "constraints": { "min": 0, "max": 512 },
                "value": null, "default": default, "doc": doc,
            }))
            .expect("an int row parses")
        };
        let v = row("general.gaps-out-vertical", Value::Null, "Gap, logical px.");
        assert_eq!(v.mirror_of(), Some("general.gaps-out"));
        assert_eq!(v.unit(), Some("px"));
        let plain = row("general.gaps-out", json!(10), "Gap, logical px.");
        assert_eq!(plain.mirror_of(), None);
        assert_eq!(row("input.repeat-rate", json!(25), "Per second.").unit(), None);
    }

    #[test]
    fn a_label_is_the_last_segment_unless_it_names_a_thing() {
        let row = |path: &str| {
            Row::parse(&json!({
                "path": path, "file": "abyss", "type": "bool", "value": true,
            }))
            .expect("a bool row parses")
        };
        assert_eq!(row("bar.fold-when-idle").label(), "Fold when idle");
        assert_eq!(
            row("input.focus-follows-mouse-across-outputs").label(),
            "Focus follows mouse across outputs"
        );
        assert_eq!(row("general.gaps-in").label(), "Between windows");
        assert_eq!(row("bar.eye").label(), "Eye indicator");
        assert_eq!(row("mode").label(), "Interaction mode");
        assert_eq!(row("components.control-center").label(), "Control center");
        assert_eq!(row("decoration.blur.glass.rim").label(), "Glass rim light");
        assert_eq!(row("decoration.blur.frost.tint").label(), "Frost tint");
        assert_eq!(row("general.col-active-border").label(), "Active border colour");
        assert_eq!(row("input.repeat-rate").label(), "Key repeat rate");
    }

    #[test]
    fn every_label_starts_with_a_capital() {
        for path in [
            "decoration.blur.glass.refraction",
            "decoration.blur.glass.bevel",
            "decoration.blur.glass.dispersion",
            "decoration.blur.glass.rim",
            "decoration.blur.frost.tint",
            "general.col-inactive-border",
            "decoration.dim-inactive",
        ] {
            let row = Row::parse(&json!({"path": path, "file": "abyss", "type": "bool"})).expect("parses");
            let label = row.label();
            assert!(label.starts_with(char::is_uppercase), "{path} reads {label:?}");
        }
    }

    #[test]
    fn a_moded_key_applies_by_its_sub_node() {
        let modes: Vec<String> = ["off", "blur", "frost", "glass"].map(String::from).into();
        assert_eq!(moded(BLUR, "decoration.blur.mode", &modes), None);
        assert_eq!(moded(BLUR, "decoration.rounding", &modes), None);
        assert_eq!(
            moded(BLUR, "decoration.blur.size", &modes),
            Some(vec!["blur", "frost", "glass"])
        );
        assert_eq!(
            moded(BLUR, "decoration.blur.frost.tint", &modes),
            Some(vec!["frost"])
        );
        assert_eq!(
            moded(BLUR, "decoration.blur.glass.rim", &modes),
            Some(vec!["glass"])
        );
        // A sub-node no mode is named for applies to no mode, and says so by
        // being dimmed under every one of them rather than by vanishing.
        assert_eq!(moded(BLUR, "decoration.blur.smoke.depth", &modes), Some(vec![]));
    }

    #[test]
    fn enum_values_read_as_words_or_as_themselves() {
        assert_eq!(value_label("cell"), "below chip");
        assert_eq!(value_label("pointer"), "at pointer");
        assert_eq!(value_label("ease-out"), "ease-out");
        assert_eq!(value_label("radiant"), "Radiant (Default)");
        assert_eq!(value_label("dwindle"), "Dwindle Classic");
        assert_eq!(value_label("de"), "Desktop");
        let blur: Vec<&str> = ["off", "blur", "frost", "glass"]
            .into_iter()
            .map(value_label)
            .collect();
        assert_eq!(blur, ["Off", "Blur", "Frost", "Liquid Glass"]);
    }

    #[test]
    fn a_colour_parses_with_or_without_alpha() {
        assert_eq!(rgba("#e8a33dff"), Some([0xe8, 0xa3, 0x3d, 0xff]));
        assert_eq!(rgba("#e8a33d"), Some([0xe8, 0xa3, 0x3d, 0xff]));
        assert_eq!(rgba("#e8a33d8"), None);
        assert_eq!(rgba("e8a33dff"), None);
        assert_eq!(rgba("#zz0000"), None);
    }

    #[test]
    fn a_number_without_bounds_has_no_slider() {
        assert_eq!(control_for("int", &Value::Null), None);
    }

    #[test]
    fn drag_max_is_optional_and_falls_back_to_max() {
        let plain = control_for("int", &json!({"min": 0, "max": 512}));
        assert_eq!(
            plain,
            Some(Control::Slider {
                min: 0.0,
                max: 512.0,
                integral: true,
                drag_max: None
            })
        );
        let narrowed = control_for("int", &json!({"min": 0, "max": 512, "drag-max": 32}));
        assert_eq!(
            narrowed,
            Some(Control::Slider {
                min: 0.0,
                max: 512.0,
                integral: true,
                drag_max: Some(32.0)
            })
        );
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
