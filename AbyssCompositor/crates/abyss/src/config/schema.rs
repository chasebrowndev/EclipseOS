// SPDX-License-Identifier: AGPL-3.0-only
//! The declarative description of every config key (COMP-13 §1.4, F-01 §4).
//!
//! F-01 §4 requires every setting to be reachable without a text editor. A GUI
//! cannot generate a control for a key it does not know the type, range and
//! owning file of — so that knowledge has to exist as data, in one place, and
//! it has to be impossible for the parser and the table to drift apart.
//!
//! This is a static table, deliberately, not a derive macro: `abyss` carries no
//! proc-macro dependency, and a macro would hide ~50 rows that a human reviewer
//! needs to read one by one — the `owner` column decides which keys live in the
//! file the control socket may never write.
//!
//! Drift is caught from three sides, in `tests` below:
//! 1. schema → parser: every key in [`TABLE`] is accepted by `Config::apply`.
//! 2. parser → schema: the parser's `unknown …` fallthrough consults the table,
//!    so a key the parser handles but the table omits is reported as a bug.
//! 3. schema → defaults: [`get`] on a default `Config` returns exactly the
//!    `default` column for every row.

use super::{Config, LayoutKind};

/// The type of a key's value, and whatever constrains it. A GUI maps this
/// straight onto a control: `Bool` is a toggle, `Int{min,max}` a slider,
/// `Enum` a row of pills.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ty {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
    },
    Str,
    Enum(&'static [&'static str]),
    /// `#rrggbb` / `#rrggbbaa`.
    Color,
    /// A single node carrying any number of string arguments.
    StrList,
}

/// Which file a key lives in (COMP-13 §1.3). `Policy` keys are the security
/// surface: the control socket refuses to write them at all, and a GUI shows
/// them read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Abyss,
    Policy,
}

/// Whether a written value takes effect on the next reload or only on restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    Live,
    NeedsRestart,
}

/// A key's value, as read back out of a live [`Config`]. `Null` is a key that
/// is genuinely unset, not an error.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<String>),
    Color([f32; 4]),
}

/// A default, in a form that can be written in a `const`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dv {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'static str),
    /// Every list-valued key defaults to empty; a non-empty default would be
    /// an allow-list entry nobody asked for.
    EmptyList,
    Color([f32; 4]),
}

/// One row. `path` is dotted and mirrors the KDL nesting exactly, so
/// `decoration.blur.size` is `decoration { blur { size … } }`.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub path: &'static str,
    pub ty: Ty,
    pub default: Dv,
    pub owner: Owner,
    pub reload: Reload,
    pub doc: &'static str,
}

const fn k(path: &'static str, ty: Ty, default: Dv, owner: Owner, reload: Reload, doc: &'static str) -> Key {
    Key {
        path,
        ty,
        default,
        owner,
        reload,
        doc,
    }
}

use Dv::*;
use Owner::{Abyss, Policy};
use Reload::{Live, NeedsRestart};

const PCT: Ty = Ty::Float { min: 0.0, max: 1.0 };
const fn int(min: i64, max: i64) -> Ty {
    Ty::Int { min, max }
}

/// Every scalar key `abyss` understands.
///
/// `xwayland.enable` is `Abyss` and not `Policy` even though disabling X11 is
/// the strongest isolation available (ADR 0026): splitting one two-key block
/// across two files costs more in confusion than it buys, and the X11 trust
/// domain is already clamped per-window by the `app-trust`/`seat-compat`
/// actions, which *are* policy-owned. Stated so the choice is visible rather
/// than inferred.
pub const TABLE: &[Key] = &[
    // general
    k(
        "general.gaps-in",
        int(0, 512),
        Int(5),
        Abyss,
        Live,
        "Gap between tiled windows, logical px.",
    ),
    k(
        "general.gaps-out",
        int(0, 512),
        Int(10),
        Abyss,
        Live,
        "Gap between the tiling area and the screen edge, logical px.",
    ),
    k(
        "general.border-size",
        int(0, 512),
        Int(2),
        Abyss,
        Live,
        "Window border thickness, logical px. 0 disables borders.",
    ),
    k(
        "general.layout",
        Ty::Enum(&["dwindle", "master"]),
        Str("dwindle"),
        Abyss,
        Live,
        "Default tiling layout for workspaces without their own.",
    ),
    k(
        "general.focus-follows-mouse",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Move keyboard focus to the window under the pointer.",
    ),
    k(
        "general.col-active-border",
        Ty::Color,
        Color([0.91, 0.64, 0.24, 1.0]),
        Abyss,
        Live,
        "Border colour of the focused window.",
    ),
    k(
        "general.col-inactive-border",
        Ty::Color,
        Color([0.09, 0.09, 0.09, 1.0]),
        Abyss,
        Live,
        "Border colour of every unfocused window.",
    ),
    // render
    k(
        "render.direct-scanout",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Let the DRM backend hand buffers straight to a KMS plane. Composition \
       is forced anyway for any frame holding a sensitive surface.",
    ),
    // decoration
    k(
        "decoration.rounding",
        int(0, 512),
        Int(0),
        Abyss,
        Live,
        "Corner radius in logical px; 0 disables.",
    ),
    k(
        "decoration.active-opacity",
        PCT,
        Float(1.0),
        Abyss,
        Live,
        "Alpha applied to the focused window.",
    ),
    k(
        "decoration.inactive-opacity",
        PCT,
        Float(1.0),
        Abyss,
        Live,
        "Alpha applied to every unfocused window.",
    ),
    k(
        "decoration.dim-inactive",
        PCT,
        Float(0.0),
        Abyss,
        Live,
        "Strength of the darkening overlay on unfocused windows.",
    ),
    k(
        "decoration.blur.enabled",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Dual-Kawase blur behind translucent windows.",
    ),
    k(
        "decoration.blur.size",
        int(1, 64),
        Int(8),
        Abyss,
        Live,
        "Blur kernel offset. Larger is softer and costs more.",
    ),
    k(
        "decoration.blur.passes",
        int(1, 6),
        Int(2),
        Abyss,
        Live,
        "Down/up-sample pairs in the blur chain.",
    ),
    k(
        "decoration.shadow.enabled",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Drop shadow behind windows.",
    ),
    k(
        "decoration.shadow.range",
        int(0, 128),
        Int(20),
        Abyss,
        Live,
        "Shadow falloff distance, logical px.",
    ),
    // animations
    k(
        "animations.enabled",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Master switch for geometry animations.",
    ),
    // xwayland
    k(
        "xwayland.enable",
        Ty::Bool,
        Bool(true),
        Abyss,
        NeedsRestart,
        "Run an X server. Off is the strongest isolation available (ADR 0026).",
    ),
    k(
        "xwayland.scaling",
        Ty::Enum(&["compositor", "client"]),
        Str("compositor"),
        Abyss,
        NeedsRestart,
        "`compositor` upscales X11 clients — blurry but always correct. \
       `client` hands them DPI hints and lets them render natively.",
    ),
    // idle
    k(
        "idle.dpms-timeout-seconds",
        int(0, 86_400),
        Null,
        Abyss,
        Live,
        "Inactivity before outputs power off. Unset or 0 disables.",
    ),
    k(
        "idle.lock-timeout-seconds",
        int(0, 86_400),
        Null,
        Abyss,
        Live,
        "Inactivity before `idle.lock-command` runs. Unset or 0 disables.",
    ),
    k(
        "idle.lock-command",
        Ty::Str,
        Null,
        Abyss,
        Live,
        "Locker to spawn on the lock timeout. Without one the timeout is inert.",
    ),
    // input
    k("input.kb-layout", Ty::Str, Str("us"), Abyss, Live, "XKB layout."),
    k("input.kb-variant", Ty::Str, Str(""), Abyss, Live, "XKB variant."),
    k(
        "input.kb-options",
        Ty::Str,
        Null,
        Abyss,
        Live,
        "XKB options string.",
    ),
    k(
        "input.repeat-rate",
        int(0, 255),
        Int(40),
        Abyss,
        Live,
        "Key repeats per second.",
    ),
    k(
        "input.repeat-delay",
        int(0, 5_000),
        Int(300),
        Abyss,
        Live,
        "Milliseconds held before a key starts repeating.",
    ),
    k(
        "input.accel-profile",
        Ty::Enum(&["adaptive", "flat"]),
        Str("adaptive"),
        Abyss,
        Live,
        "Pointer acceleration profile.",
    ),
    k(
        "input.touchpad.natural-scroll",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Invert touchpad scroll direction.",
    ),
    k(
        "input.touchpad.tap-to-click",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Treat a tap as a click.",
    ),
    k(
        "input.touchpad.dwt",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Disable the touchpad while typing.",
    ),
    // misc
    k(
        "misc.render-device",
        Ty::Str,
        Null,
        Abyss,
        NeedsRestart,
        "`auto`, a /dev/dri/… path, or `pci:DDDD:BB:DD.F`. The CLI flag and \
       ECLIPSE_RENDER_DEVICE both override it.",
    ),
    // --- policy-owned: the security surface -------------------------------
    k(
        "misc.scripted-input",
        Ty::Bool,
        Bool(false),
        Policy,
        Live,
        "Let the control socket synthesise human input (COMP-13 §2.2).",
    ),
    k(
        "clipboard.data-control-allow",
        Ty::StrList,
        EmptyList,
        Policy,
        Live,
        "Process names allowed to bind zwlr_data_control_manager_v1. Empty \
       denies everyone — data control reads every selection.",
    ),
    k(
        "capture.allow",
        Ty::StrList,
        EmptyList,
        Policy,
        Live,
        "Process names allowed to bind zwlr_screencopy_manager_v1. Empty \
       denies everyone.",
    ),
    k(
        "capture.redact-app-id",
        Ty::StrList,
        EmptyList,
        Policy,
        Live,
        "app_ids whose windows are `secret`: never composited into a capture \
       target, only a solid placeholder (COMP-02 §7).",
    ),
];

/// Owner of a whole top-level node, when every key under it agrees.
///
/// `None` means either "not in the schema" or "mixed" — `misc` holds
/// `render-device` (abyss) beside `scripted-input` (policy), and `windowrule`
/// is decided per action. Both are checked one key at a time instead.
pub fn node_owner(node: &str) -> Option<Owner> {
    if let Some(c) = COLLECTIONS.iter().find(|c| c.node == node) {
        // windowrule's owner is its action's; see RULE_ACTIONS.
        return if c.node == "windowrule" {
            None
        } else {
            Some(c.owner)
        };
    }
    let prefix = format!("{node}.");
    let mut owners = TABLE
        .iter()
        .filter(|k| k.path.starts_with(&prefix))
        .map(|k| k.owner);
    let first = owners.next()?;
    owners.all(|o| o == first).then_some(first)
}

pub fn get_key(path: &str) -> Option<&'static Key> {
    TABLE.iter().find(|k| k.path == path)
}

/// A repeating construct: several nodes of the same name, each with its own
/// identity. These are not settable through `set_config_value` v1 — editing one
/// means naming *which* one, which is list-identity semantics and its own work
/// item — so they are listed here for the GUI to render and for
/// `ci/gui-coverage-exceptions.txt` to point at.
#[derive(Debug, Clone, Copy)]
pub struct Collection {
    pub node: &'static str,
    pub owner: Owner,
    pub doc: &'static str,
}

pub const COLLECTIONS: &[Collection] = &[
    Collection { node: "bind", owner: Abyss, doc: "A key binding." },
    Collection { node: "output", owner: Abyss, doc: "Per-output mode, position, scale, overscan." },
    Collection { node: "workspace", owner: Abyss, doc: "Per-workspace layout override." },
    Collection { node: "windowrule", owner: Abyss, doc: "A rule matched against windows at map time. Its *action* decides the owning file — see RULE_ACTIONS." },
];

/// `windowrule` is the one construct whose criticality is mixed: `float` is
/// cosmetic, `sensitivity` is the redaction class. So ownership is per-action
/// data rather than per-node, and `apply_windowrule` refuses in both directions.
///
/// `no-agent` is policy-owned: it decides whether an agent can see a window at
/// all, which is the same kind of decision as `sensitivity`.
pub const RULE_ACTIONS: &[(&str, Owner)] = &[
    ("float", Abyss),
    ("tile", Abyss),
    ("fullscreen", Abyss),
    ("size", Abyss),
    ("position", Abyss),
    ("output", Abyss),
    ("opacity", Abyss),
    ("workspace", Abyss),
    ("no-focus-steal", Abyss),
    ("idle-inhibit", Abyss),
    ("sensitivity", Policy),
    ("app-trust", Policy),
    ("seat-compat", Policy),
    ("no-agent", Policy),
];

pub fn rule_owner(action: &str) -> Option<Owner> {
    RULE_ACTIONS.iter().find(|(a, _)| *a == action).map(|(_, o)| *o)
}

/// Read a key back out of a live `Config`. `None` means the path is not in the
/// schema at all; `Some(Value::Null)` means it is a key that is currently unset.
///
/// This is the other half of the write path: `get_config` answers with it, and
/// the drift test below asserts it agrees with the `default` column on a
/// default `Config` for every single row.
pub fn get(c: &Config, path: &str) -> Option<Value> {
    use Value as V;
    let s = |o: &Option<String>| o.clone().map_or(V::Null, V::Str);
    let n = |o: &Option<u64>| o.map_or(V::Null, |v| V::Int(v as i64));
    let list = |v: &[String]| V::List(v.to_vec());
    Some(match path {
        "general.gaps-in" => V::Int(c.general.gaps_in as i64),
        "general.gaps-out" => V::Int(c.general.gaps_out as i64),
        "general.border-size" => V::Int(c.general.border_size as i64),
        "general.layout" => V::Str(layout_name(c.general.layout).into()),
        "general.focus-follows-mouse" => V::Bool(c.general.focus_follows_mouse),
        "general.col-active-border" => V::Color(c.general.col_active),
        "general.col-inactive-border" => V::Color(c.general.col_inactive),
        "render.direct-scanout" => V::Bool(c.render.direct_scanout),
        "decoration.rounding" => V::Int(c.decoration.rounding as i64),
        "decoration.active-opacity" => V::Float(c.decoration.active_opacity as f64),
        "decoration.inactive-opacity" => V::Float(c.decoration.inactive_opacity as f64),
        "decoration.dim-inactive" => V::Float(c.decoration.dim_inactive as f64),
        "decoration.blur.enabled" => V::Bool(c.decoration.blur.enabled),
        "decoration.blur.size" => V::Int(c.decoration.blur.size as i64),
        "decoration.blur.passes" => V::Int(c.decoration.blur.passes as i64),
        "decoration.shadow.enabled" => V::Bool(c.decoration.shadow.enabled),
        "decoration.shadow.range" => V::Int(c.decoration.shadow.range as i64),
        "animations.enabled" => V::Bool(c.animations.enabled),
        "xwayland.enable" => V::Bool(c.xwayland.enable),
        "xwayland.scaling" => V::Str(
            if c.xwayland.scaling_client {
                "client"
            } else {
                "compositor"
            }
            .into(),
        ),
        "idle.dpms-timeout-seconds" => n(&c.idle.dpms_timeout),
        "idle.lock-timeout-seconds" => n(&c.idle.lock_timeout),
        "idle.lock-command" => s(&c.idle.lock_command),
        "input.kb-layout" => V::Str(c.input.kb_layout.clone()),
        "input.kb-variant" => V::Str(c.input.kb_variant.clone()),
        "input.kb-options" => s(&c.input.kb_options),
        "input.repeat-rate" => V::Int(c.input.repeat_rate as i64),
        "input.repeat-delay" => V::Int(c.input.repeat_delay as i64),
        "input.accel-profile" => V::Str(c.input.accel_profile.clone()),
        "input.touchpad.natural-scroll" => V::Bool(c.input.touchpad.natural_scroll),
        "input.touchpad.tap-to-click" => V::Bool(c.input.touchpad.tap_to_click),
        "input.touchpad.dwt" => V::Bool(c.input.touchpad.dwt),
        "misc.render-device" => s(&c.misc.render_device),
        "misc.scripted-input" => V::Bool(c.misc.scripted_input),
        "clipboard.data-control-allow" => list(&c.clipboard.data_control_allow),
        "capture.allow" => list(&c.capture.allow),
        "capture.redact-app-id" => list(&c.capture.redact_app_id),
        _ => return None,
    })
}

fn layout_name(l: LayoutKind) -> &'static str {
    match l {
        LayoutKind::Dwindle => "dwindle",
        LayoutKind::Master => "master",
    }
}

impl Dv {
    /// The default as a [`Value`], so it can be compared with [`get`].
    pub fn as_value(self) -> Value {
        match self {
            Dv::Null => Value::Null,
            Dv::Bool(b) => Value::Bool(b),
            Dv::Int(i) => Value::Int(i),
            Dv::Float(f) => Value::Float(f),
            Dv::Str(s) => Value::Str(s.to_string()),
            Dv::EmptyList => Value::List(Vec::new()),
            Dv::Color(c) => Value::Color(c),
        }
    }

    /// The default written as KDL source, for `config describe` and for
    /// synthesising a config in the drift test.
    pub fn to_kdl(self) -> Option<String> {
        Some(match self {
            Dv::Null => return None,
            Dv::Bool(b) => format!("#{b}"),
            Dv::Int(i) => i.to_string(),
            Dv::Float(f) => format!("{f:?}"),
            Dv::Str(s) => format!("{s:?}"),
            Dv::EmptyList => String::new(),
            Dv::Color(c) => format!(
                "\"#{:02x}{:02x}{:02x}{:02x}\"",
                (c[0] * 255.0).round() as u8,
                (c[1] * 255.0).round() as u8,
                (c[2] * 255.0).round() as u8,
                (c[3] * 255.0).round() as u8
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::edit;
    use kdl::KdlDocument;

    /// Build a config file that sets every scalar key to a value of the right
    /// shape, using the real write path to do it.
    fn synthesised() -> String {
        let mut text = String::new();
        for key in TABLE {
            let Some(v) = sample(key) else { continue };
            text = edit::set_value(&text, key.path, &v).unwrap_or_else(|e| panic!("{}: {e}", key.path));
        }
        text
    }

    /// A legal, non-default value for a key — used to prove the parser accepts
    /// the key *and* that it lands where `get` says it does.
    fn sample(key: &Key) -> Option<kdl::KdlValue> {
        use kdl::KdlValue as K;
        Some(match key.ty {
            Ty::Bool => K::Bool(!matches!(key.default, Dv::Bool(true))),
            Ty::Int { min, max } => K::Integer((min + 1).min(max) as i128),
            Ty::Float { min, max } => K::Float((min + max) / 2.0),
            Ty::Enum(vs) => K::String(vs[vs.len() - 1].to_string()),
            Ty::Color => K::String("#0a0b0c0d".into()),
            Ty::Str => K::String(match key.path {
                // Not free-form: each of these is validated on its own terms.
                // "auto" is normalised to `None` by the parser — it means
                // "ask smithay", which is the unset state, not a value.
                "misc.render-device" => "/dev/dri/card1".into(),
                "input.kb-layout" => "de".into(),
                _ => "x".to_string(),
            }),
            // A list node is not a scalar assignment; covered by its own row on
            // the GUI-coverage exception list, not by the splice path.
            Ty::StrList => return None,
        })
    }

    /// Side 1: every key the schema claims exists is a key the parser accepts.
    /// A typo in `path`, or a key deleted from the parser, fails here.
    #[test]
    fn every_schema_key_is_accepted_by_the_parser() {
        let text = synthesised();
        let doc: KdlDocument = text.parse().expect("synthesised config parses");
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(
            cfg.errors.is_empty(),
            "parser rejected schema-generated config:\n{}\n{:#?}",
            text,
            cfg.errors
        );
    }

    /// …and the value lands in the field `get` reads, so the two halves of the
    /// round trip name the same thing.
    #[test]
    fn a_written_value_reads_back_through_get() {
        let text = synthesised();
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        for key in TABLE {
            let Some(want) = sample(key) else { continue };
            let got = get(&cfg, key.path).unwrap_or_else(|| panic!("get({}) is None", key.path));
            let ok = match (&got, &want) {
                (Value::Bool(a), kdl::KdlValue::Bool(b)) => a == b,
                (Value::Int(a), kdl::KdlValue::Integer(b)) => *a as i128 == *b,
                (Value::Float(a), kdl::KdlValue::Float(b)) => (a - b).abs() < 1e-6,
                (Value::Str(a), kdl::KdlValue::String(b)) => a == b,
                (Value::Color(_), kdl::KdlValue::String(_)) => true,
                _ => false,
            };
            assert!(ok, "{}: wrote {want}, read back {got:?}", key.path);
        }
    }

    /// Side 3: `get` agrees with the `default` column on a default `Config`.
    /// This is what lets `get_config` report `source: null` for a key at its
    /// default without consulting any file.
    #[test]
    fn get_agrees_with_the_default_column() {
        let cfg = Config::default();
        for key in TABLE {
            let got = get(&cfg, key.path).unwrap_or_else(|| panic!("{} missing from get", key.path));
            assert_eq!(got, key.default.as_value(), "default drift at {}", key.path);
        }
    }

    /// Paths are unique, dotted, and never empty-segmented — `get` and the
    /// splice path both index on them.
    #[test]
    fn paths_are_well_formed_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for key in TABLE {
            assert!(seen.insert(key.path), "duplicate path {}", key.path);
            assert!(
                key.path.split('.').all(|s| !s.is_empty()),
                "malformed path {}",
                key.path
            );
        }
        for (a, _) in RULE_ACTIONS {
            assert!(seen.insert(a), "rule action collides with a key path: {a}");
        }
    }

    /// The security surface is small and enumerated. A key moving into or out
    /// of `policy.kdl` is a decision with a review attached, not a diff nobody
    /// notices — so the list is pinned here.
    #[test]
    fn the_policy_owned_set_is_exactly_this() {
        let policy: Vec<_> = TABLE
            .iter()
            .filter(|k| k.owner == Policy)
            .map(|k| k.path)
            .collect();
        assert_eq!(
            policy,
            [
                "misc.scripted-input",
                "clipboard.data-control-allow",
                "capture.allow",
                "capture.redact-app-id",
            ]
        );
        let rules: Vec<_> = RULE_ACTIONS
            .iter()
            .filter(|(_, o)| *o == Policy)
            .map(|(a, _)| *a)
            .collect();
        assert_eq!(rules, ["sensitivity", "app-trust", "seat-compat", "no-agent"]);
    }
}
