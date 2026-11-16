// SPDX-License-Identifier: AGPL-3.0-only
//! Row and value types of the schema (T-10 §1).
//!
//! These wrap `ec-config-schema` and add what X1 will eventually move there: `Ty::Argv`,
//! `CodepointRanges`, `Match`, `StringSet`, `Map`, `ThemeRef` (plus the local `Adjust`),
//! `Reload::NewPane`/`NewWindow`, and `Dv::Computed`. Every local addition maps to the
//! nearest `ec_config_schema::Ty` through [`Ty::generic`] so a generic renderer still works.

use ec_config_schema as ecs;

/// The type of a key's value and whatever constrains it.
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
    Color,
    StrList,
    Font,
    FontFeatures,
    KeyChord,
    Duration {
        min_ms: u64,
        max_ms: u64,
    },
    Path,
    Palette16,
    Action,
    Regex,
    Template,
    Bytes {
        min: u64,
        max: u64,
    },
    // ---- local additions (X1 follow-up) ----
    /// argv array with the T-11 §2 typed placeholders. Carried as `Value::List`.
    Argv,
    /// A list of codepoint ranges (`U+E0A0-E0A3`, `U+F000`). Carried as `Value::List`.
    CodepointRanges,
    /// A T-11 §7 match expression. Carried as `Value::Str`.
    Match,
    /// An unordered set of strings, duplicates dropped. Carried as `Value::List`.
    StringSet,
    /// Child nodes: node name is the key, arguments are the value(s). Carried as `Value::Map`.
    Map {
        key: &'static Ty,
        value: &'static Ty,
    },
    /// One scheme (`system` or a name) or a `light=… dark=…` pair. Carried as `Value::Str`
    /// in the canonical text form (`system`, `Name`, `light=A dark=B`).
    ThemeRef,
    /// A signed length with an explicit unit: `3px`, `-10%`. Carried as `Value::Str`.
    Adjust,
}

impl Ty {
    pub const fn int(min: i64, max: i64) -> Ty {
        Ty::Int { min, max }
    }
    pub const fn float(min: f64, max: f64) -> Ty {
        Ty::Float { min, max }
    }
    pub const fn dur(min_ms: u64, max_ms: u64) -> Ty {
        Ty::Duration { min_ms, max_ms }
    }
    pub const fn bytes(min: u64, max: u64) -> Ty {
        Ty::Bytes { min, max }
    }

    /// The nearest `ec-config-schema` type, for generic rendering.
    pub fn generic(&self) -> ecs::Ty {
        match *self {
            Ty::Bool => ecs::Ty::Bool,
            Ty::Int { min, max } => ecs::Ty::Int { min, max },
            Ty::Float { min, max } => ecs::Ty::Float { min, max },
            Ty::Str => ecs::Ty::Str,
            Ty::Enum(v) => ecs::Ty::Enum(v),
            Ty::Color => ecs::Ty::Color,
            Ty::StrList => ecs::Ty::StrList,
            Ty::Font => ecs::Ty::Font,
            Ty::FontFeatures => ecs::Ty::FontFeatures,
            Ty::KeyChord => ecs::Ty::KeyChord,
            Ty::Duration { min_ms, max_ms } => ecs::Ty::Duration { min_ms, max_ms },
            Ty::Path => ecs::Ty::Path,
            Ty::Palette16 => ecs::Ty::Palette16,
            Ty::Action => ecs::Ty::Action,
            Ty::Regex => ecs::Ty::Regex,
            Ty::Template => ecs::Ty::Template,
            Ty::Bytes { min, max } => ecs::Ty::Bytes { min, max },
            Ty::Argv | Ty::CodepointRanges | Ty::StringSet | Ty::Map { .. } => ecs::Ty::StrList,
            Ty::Match | Ty::ThemeRef | Ty::Adjust => ecs::Ty::Str,
        }
    }

    /// The ec-settings widget a row of this type uses. The names for the local additions are
    /// the X1 contract (T-10 §1: "each new variant names its widget").
    pub fn widget(&self) -> &'static str {
        match self {
            Ty::Bool => "toggle",
            Ty::Int { .. } | Ty::Float { .. } => "slider",
            Ty::Str => "text-field",
            Ty::Enum(_) => "pills",
            Ty::Color => "color-picker",
            Ty::StrList => "tag-list",
            Ty::Font => "font-picker",
            Ty::FontFeatures => "feature-tag-editor",
            Ty::KeyChord => "chord-capture",
            Ty::Duration { .. } => "duration-slider",
            Ty::Path => "path-field",
            Ty::Palette16 => "palette-grid",
            Ty::Action => "action-picker",
            Ty::Regex => "regex-field",
            Ty::Template => "template-field",
            Ty::Bytes { .. } => "bytes-slider",
            Ty::Argv => "argv-editor",
            Ty::CodepointRanges => "range-list-editor",
            Ty::Match => "match-editor",
            Ty::StringSet => "chip-set",
            Ty::Map { .. } => "key-value-editor",
            Ty::ThemeRef => "theme-picker",
            Ty::Adjust => "adjust-field",
        }
    }

    /// Whether the KDL form is a node with several arguments.
    pub fn is_arg_list(&self) -> bool {
        matches!(
            self,
            Ty::StrList
                | Ty::FontFeatures
                | Ty::Palette16
                | Ty::Argv
                | Ty::CodepointRanges
                | Ty::StringSet
        )
    }

    /// Types that force the `security` override class (T-10 §1.1).
    pub fn forces_security(&self) -> bool {
        matches!(self, Ty::Argv | Ty::Action | Ty::Match | Ty::Map { .. })
    }
}

/// How soon a changed value takes effect. Abyss maps `Live` to `Live` and `NeedsRestart` to
/// `Restart`; the two new variants exist only here until X1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reload {
    Live,
    /// Applies to panes created after the change.
    NewPane,
    /// Applies to windows created after the change.
    NewWindow,
    Restart,
}

impl Reload {
    pub fn generic(self) -> ecs::Reload {
        match self {
            Reload::Live => ecs::Reload::Live,
            _ => ecs::Reload::NeedsRestart,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Reload::Live => "live",
            Reload::NewPane => "new-pane",
            Reload::NewWindow => "new-window",
            Reload::Restart => "restart",
        }
    }
}

/// Where a key may be set: the narrowest object that can hold its own value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Global,
    Window,
    Tab,
    Pane,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Window => "window",
            Scope::Tab => "tab",
            Scope::Pane => "pane",
        }
    }
}

/// Override class (T-10 §1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideClass {
    Appearance,
    Behaviour,
    Security,
    Grant,
}

impl OverrideClass {
    pub fn name(self) -> &'static str {
        match self {
            OverrideClass::Appearance => "appearance",
            OverrideClass::Behaviour => "behaviour",
            OverrideClass::Security => "security",
            OverrideClass::Grant => "grant",
        }
    }

    /// Parse a class name; anything unknown is `Grant` (fail-closed, T-10 §1.1).
    pub fn parse_fail_closed(s: &str) -> OverrideClass {
        match s {
            "appearance" => OverrideClass::Appearance,
            "behaviour" => OverrideClass::Behaviour,
            "security" => OverrideClass::Security,
            _ => OverrideClass::Grant,
        }
    }

    /// Does layer 8 (`config.set`) accept it?
    pub fn runtime_override_allowed(self) -> bool {
        matches!(self, OverrideClass::Appearance | OverrideClass::Behaviour)
    }

    /// The T-12 §4 socket class `config.set` needs, if the override is allowed at all.
    pub fn runtime_socket_class(self) -> Option<&'static str> {
        match self {
            OverrideClass::Appearance => Some("appearance"),
            OverrideClass::Behaviour => Some("config-write"),
            _ => None,
        }
    }

    /// The T-12 §4 socket class `config.persist` needs, if persisting over the socket is
    /// allowed at all (`grant` never is).
    pub fn persist_socket_class(self) -> Option<&'static str> {
        match self {
            OverrideClass::Grant => None,
            _ => Some("config-write"),
        }
    }
}

/// A runtime value. `Null` is an unset optional key (or a `Computed` default).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<String>),
    /// `#rrggbbaa`.
    Color([u8; 4]),
    /// Node name -> argument strings.
    Map(Vec<(String, Vec<String>)>),
}

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[String]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }
    pub fn as_color(&self) -> Option<[u8; 4]> {
        match self {
            Value::Color(c) => Some(*c),
            _ => None,
        }
    }
    pub fn as_map(&self) -> Option<&[(String, Vec<String>)]> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

/// A default, writable in a `const`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dv {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'static str),
    /// Empty list or empty map.
    Empty,
    List(&'static [&'static str]),
    /// `0xRRGGBB`, opaque.
    Rgb(u32),
    /// `0xRRGGBBAA`.
    Rgba(u32),
    /// A named function of the runtime environment (T-10 §1). Shown as "Automatic
    /// (currently X)"; reset restores it; `config diff` compares against it.
    Computed {
        id: &'static str,
        doc: &'static str,
    },
}

impl Dv {
    /// The default as a value. `Computed` is `Null`: the environment fills it in.
    pub fn value(self) -> Value {
        match self {
            Dv::Null | Dv::Computed { .. } => Value::Null,
            Dv::Bool(b) => Value::Bool(b),
            Dv::Int(i) => Value::Int(i),
            Dv::Float(f) => Value::Float(f),
            Dv::Str(s) => Value::Str(s.to_string()),
            Dv::Empty => Value::Null,
            Dv::List(l) => Value::List(l.iter().map(|s| s.to_string()).collect()),
            Dv::Rgb(c) => Value::Color([(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]),
            Dv::Rgba(c) => {
                Value::Color([(c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8])
            }
        }
    }

    pub fn is_computed(self) -> bool {
        matches!(self, Dv::Computed { .. })
    }
}

/// One row of [`crate::schema::TABLE`].
#[derive(Debug, Clone, Copy)]
pub struct Key {
    /// Dotted path mirroring the KDL nesting (`font.builtin-glyphs.box`).
    pub path: &'static str,
    pub ty: Ty,
    pub default: Dv,
    pub scope: Scope,
    /// May a profile carry this key (T-11 §6)?
    pub profilable: bool,
    pub reload: Reload,
    pub class: OverrideClass,
    pub doc: &'static str,
}

impl Key {
    /// The default as a value, with the type's empty form for `Dv::Empty`.
    pub fn default_value(&self) -> Value {
        match (self.default, self.ty) {
            (Dv::Empty, Ty::Map { .. }) => Value::Map(Vec::new()),
            (Dv::Empty, t) if t.is_arg_list() => Value::List(Vec::new()),
            (d, _) => d.value(),
        }
    }
}

/// Release tier (T-00 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    R1,
    R2,
    R3,
}

/// One key's presentation (T-10 §1: label, page, widget, group, keywords, tier). The widget is
/// a function of the row's [`Ty`] ([`Ty::widget`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub path: &'static str,
    pub label: &'static str,
    /// `section/page`.
    pub page: &'static str,
    pub group: &'static str,
    pub caption: Option<&'static str>,
    pub keywords: &'static [&'static str],
    pub tier: Tier,
}

impl Meta {
    /// A row whose page and group come from its node ([`page_of`]).
    pub const fn new(
        path: &'static str,
        label: &'static str,
        keywords: &'static [&'static str],
    ) -> Meta {
        let (page, group) = page_of(path);
        Meta {
            path,
            label,
            page,
            group,
            caption: None,
            keywords,
            tier: Tier::R1,
        }
    }

    /// The `ec-config-schema` form, for the existing generic Settings renderer.
    pub fn generic(&self) -> ecs::Meta {
        let mut m = ecs::Meta::new(self.path, self.label, self.page, self.group);
        m.caption = self.caption;
        m.keywords = self.keywords;
        m
    }
}

const NODE_PAGES: &[(&str, &str, &str)] = &[
    ("font", "appearance/fonts", "Font"),
    ("colors", "appearance/colors", "Colors"),
    ("theme", "appearance/colors", "Theme"),
    ("cursor", "appearance/cursor", "Cursor"),
    ("window", "appearance/window", "Window"),
    ("interface", "appearance/interface", "Interface"),
    ("tabs", "appearance/tabs-panes", "Tabs"),
    ("panes", "appearance/tabs-panes", "Panes"),
    ("scrollbar", "appearance/tabs-panes", "Scrollbar"),
    ("unicode", "text/unicode", "Unicode"),
    ("scrollback", "behaviour/scrollback", "Scrollback"),
    ("blocks", "behaviour/scrollback", "Blocks"),
    ("close", "behaviour/windows", "Closing"),
    ("mouse", "behaviour/mouse-keyboard", "Mouse"),
    ("touch", "behaviour/mouse-keyboard", "Touch"),
    ("keyboard", "behaviour/mouse-keyboard", "Keyboard"),
    ("selection", "behaviour/selection", "Selection"),
    ("hints", "behaviour/selection", "Hints"),
    ("bell", "behaviour/notifications", "Bell"),
    ("notifications", "behaviour/notifications", "Notifications"),
    ("progress", "behaviour/notifications", "Progress"),
    ("shell", "behaviour/shell", "Shell"),
    ("shell-integration", "behaviour/shell", "Shell integration"),
    ("terminal", "behaviour/shell", "Terminal identity"),
    ("sessions", "behaviour/sessions", "Sessions"),
    ("quick-terminal", "behaviour/windows", "Quick terminal"),
    ("graphics", "media/graphics", "Graphics"),
    ("media", "media/playback", "Media"),
    ("render", "advanced/rendering", "Rendering"),
    ("accessibility", "accessibility", "Accessibility"),
    ("clipboard", "security/clipboard", "Clipboard"),
    ("paste", "security/clipboard", "Paste"),
    ("links", "security/links", "Links"),
    ("open", "security/links", "Open with"),
    ("security", "security/policy", "Policy"),
    ("remote", "security/policy", "Remote"),
    ("socket", "security/socket", "Control socket"),
    ("binds", "keys/binds", "Binds"),
    ("mouse-binds", "keys/binds", "Mouse binds"),
    ("key-table", "keys/binds", "Key tables"),
    ("macro", "automation/macros", "Macros"),
    ("rule", "automation/rules", "Rules"),
    ("profile", "automation/profiles", "Profiles"),
    ("domain", "automation/profiles", "Domains"),
    ("layout", "automation/layouts", "Layouts"),
    ("menu", "automation/menus", "Menus"),
    ("include", "advanced/files", "Includes"),
];

const fn node_matches(path: &str, node: &str) -> bool {
    let p = path.as_bytes();
    let n = node.as_bytes();
    if p.len() < n.len() {
        return false;
    }
    let mut i = 0;
    while i < n.len() {
        if p[i] != n[i] {
            return false;
        }
        i += 1;
    }
    p.len() == n.len() || p[n.len()] == b'.'
}

/// Settings page and group for a key path, by its top-level node.
pub const fn page_of(path: &str) -> (&'static str, &'static str) {
    let mut i = 0;
    while i < NODE_PAGES.len() {
        if node_matches(path, NODE_PAGES[i].0) {
            return (NODE_PAGES[i].1, NODE_PAGES[i].2);
        }
        i += 1;
    }
    ("advanced", "Other")
}

/// The closed set of top-level nodes (T-10 §3), plus `remote` (spec gap: `remote.clients` is
/// named by T-10 §3 but `remote` is not in its node list). `open` is T-14 §11.
pub const TOP_LEVEL_NODES: &[&str] = &[
    "version",
    "font",
    "colors",
    "theme",
    "unicode",
    "window",
    "interface",
    "tabs",
    "panes",
    "close",
    "scrollbar",
    "cursor",
    "scrollback",
    "blocks",
    "mouse",
    "touch",
    "keyboard",
    "selection",
    "clipboard",
    "paste",
    "links",
    "hints",
    "shell",
    "shell-integration",
    "terminal",
    "bell",
    "notifications",
    "progress",
    "graphics",
    "media",
    "render",
    "security",
    "accessibility",
    "socket",
    "sessions",
    "domain",
    "quick-terminal",
    "binds",
    "mouse-binds",
    "key-table",
    "macro",
    "rule",
    "profile",
    "layout",
    "menu",
    "include",
    "when",
    "remote",
    "open",
];
