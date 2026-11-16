// SPDX-License-Identifier: Apache-2.0
//! Config schema types shared by every product that describes its keys as data
//! (abyss's `abyss.kdl`, Apotheosis's `apotheosis.kdl`). Types only: each
//! product owns its own table of [`Key`] rows.

#![forbid(unsafe_code)]

mod units;
pub use units::{format_bytes, format_duration, parse_bytes, parse_duration};

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
    /// A font family name. GUI: font picker. Carried as `Value::Str`.
    Font,
    /// A list of OpenType feature tags (`+liga`, `-calt`, `ss01`). GUI: a tag
    /// list editor. Carried as `Value::List`.
    FontFeatures,
    /// A key binding chord such as `ctrl+shift+t`. GUI: a press-to-record
    /// chord capture. Carried as `Value::Str`.
    KeyChord,
    /// A time span, written in KDL as `"150ms"` / `"2s"` (see
    /// [`parse_duration`]). GUI: a slider with a unit label, bounded by
    /// `min_ms..=max_ms`. Carried as `Value::Int` / `Dv::Int` milliseconds.
    Duration {
        min_ms: u64,
        max_ms: u64,
    },
    /// A filesystem path. GUI: path field with a browse button. Carried as
    /// `Value::Str`.
    Path,
    /// Sixteen `#rrggbb` colours (the ANSI palette). GUI: a 16-swatch grid.
    /// Carried as `Value::List` of 16 strings.
    Palette16,
    /// An action id plus arguments, opaque here (`"new-tab"`, `"spawn foot"`).
    /// GUI: an action picker. Carried as `Value::Str`.
    Action,
    /// A regular expression. GUI: a text field that validates as you type.
    /// Carried as `Value::Str`.
    Regex,
    /// A format template string (title, prompt, status line). GUI: a text
    /// field with placeholder help. Carried as `Value::Str`.
    Template,
    /// A byte size, written in KDL as `"50MiB"` (see [`parse_bytes`]). GUI: a
    /// slider with a unit label, bounded by `min..=max` bytes. Carried as
    /// `Value::Int` / `Dv::Int` bytes.
    Bytes {
        min: u64,
        max: u64,
    },
    /// An argv array with the T-11 §2 typed placeholders. GUI: argv editor.
    /// Carried as `Value::List`.
    Argv,
    /// A list of codepoint ranges (`U+E0A0-E0A3`, `U+F000`). GUI: range list
    /// editor. Carried as `Value::List`.
    CodepointRanges,
    /// A T-11 §7 match expression. GUI: match editor. Carried as `Value::Str`.
    Match,
    /// An unordered set of strings, duplicates dropped. GUI: chip set.
    /// Carried as `Value::List`.
    StringSet,
    /// Child nodes: node name is the key, arguments the value(s). GUI:
    /// key/value editor. Carried as `Value::Map`. The element types are
    /// `&'static` so `Ty` stays `Copy` and usable in a `const` table.
    Map {
        key: &'static Ty,
        value: &'static Ty,
    },
    /// One scheme (`system` or a name) or a `light=… dark=…` pair. GUI: theme
    /// picker. Carried as `Value::Str` in canonical text form.
    ThemeRef,
}

/// Which file a key lives in (COMP-13 §1.3). `Policy` keys are the security
/// surface: the control socket refuses to write them at all, and a GUI shows
/// them read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Abyss,
    Policy,
    /// A key of `apotheosis.kdl` (the terminal, ADR 0113). Not on the abyss
    /// control socket: `ec-settings` reads and writes the file directly.
    Terminal,
}

/// Whether a written value takes effect on the next reload or only on restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    Live,
    /// Applies to panes created after the change (terminal only).
    NewPane,
    /// Applies to windows created after the change (terminal only).
    NewWindow,
    NeedsRestart,
}

/// A key's value, as read back out of a live config. `Null` is a key that is
/// genuinely unset, not an error.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<String>),
    Color([f32; 4]),
    /// Node name -> argument strings (`Ty::Map`).
    Map(Vec<(String, Vec<String>)>),
}

/// A default, in a form that can be written in a `const`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dv {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'static str),
    /// An allow-list defaults to empty; a non-empty default there would be an
    /// entry nobody asked for.
    EmptyList,
    /// A non-empty list default. Only for ordering keys (`bar.widgets.*`),
    /// never for a policy-owned key; a test pins that in abyss.
    List(&'static [&'static str]),
    Color([f32; 4]),
    /// A named function of the runtime environment (blur capability, portal
    /// settings, compositor). Shown as "Automatic (currently X)"; reset
    /// restores it and `config diff` compares against it, not its current
    /// value. [`Dv::as_value`] is `Null`: the consumer fills it in.
    Computed {
        id: &'static str,
        doc: &'static str,
    },
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

impl Dv {
    /// The default as a [`Value`].
    pub fn as_value(self) -> Value {
        match self {
            Dv::Null | Dv::Computed { .. } => Value::Null,
            Dv::Bool(b) => Value::Bool(b),
            Dv::Int(i) => Value::Int(i),
            Dv::Float(f) => Value::Float(f),
            Dv::Str(s) => Value::Str(s.to_string()),
            Dv::EmptyList => Value::List(Vec::new()),
            Dv::List(l) => Value::List(l.iter().map(|s| s.to_string()).collect()),
            Dv::Color(c) => Value::Color(c),
        }
    }

    /// The default written as KDL source, for `config describe` and for
    /// synthesising a config in a drift test.
    pub fn to_kdl(self) -> Option<String> {
        Some(match self {
            Dv::Null | Dv::Computed { .. } => return None,
            Dv::Bool(b) => format!("#{b}"),
            Dv::Int(i) => i.to_string(),
            Dv::Float(f) => format!("{f:?}"),
            Dv::Str(s) => format!("{s:?}"),
            Dv::EmptyList => String::new(),
            Dv::List(l) => l.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(" "),
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

/// One key's presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    /// The schema path this row describes.
    pub path: &'static str,
    /// The row label in Settings and in its search results.
    pub label: &'static str,
    /// The Settings page, as `section/page` (or `section` for a section that
    /// is one page).
    pub page: &'static str,
    /// The group heading on that page.
    pub group: &'static str,
    /// The fixed line shown beneath the row, if any. A caption that depends on
    /// the key's current value is not data and stays with the app.
    pub caption: Option<&'static str>,
    /// Words people search for this key by, beyond its label, path and doc.
    /// Lowercase words joined by single spaces.
    pub keywords: &'static [&'static str],
    /// The label on the page's own pane when that pane words it differently
    /// from search and the generic list (`label`). `None`: same as `label`.
    pub pane_label: Option<&'static str>,
}

impl Meta {
    /// A row with no caption, keywords or own-pane label.
    pub const fn new(
        path: &'static str,
        label: &'static str,
        page: &'static str,
        group: &'static str,
    ) -> Meta {
        Meta {
            path,
            label,
            page,
            group,
            caption: None,
            keywords: &[],
            pane_label: None,
        }
    }

    /// This row with its own-pane wording.
    pub const fn pane(mut self, label: &'static str) -> Self {
        self.pane_label = Some(label);
        self
    }

    /// The label the page's own pane shows.
    pub fn pane_label(&self) -> &'static str {
        self.pane_label.unwrap_or(self.label)
    }
}
