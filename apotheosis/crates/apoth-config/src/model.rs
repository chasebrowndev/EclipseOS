// SPDX-License-Identifier: AGPL-3.0-only
//! The typed config: one slot per table key (value + origin), plus the collections.

use crate::chord::Chord;
use crate::schema::{self, TABLE};
use crate::types::{Key, Value};
use std::path::PathBuf;

/// The eight layers of T-10 §2, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Default = 1,
    System = 2,
    Fragment = 3,
    User = 4,
    ProfileDir = 5,
    Profile = 6,
    Launch = 7,
    Runtime = 8,
}

impl Layer {
    pub fn number(self) -> u8 {
        self as u8
    }
    pub fn name(self) -> &'static str {
        match self {
            Layer::Default => "default",
            Layer::System => "system",
            Layer::Fragment => "fragment",
            Layer::User => "user",
            Layer::ProfileDir => "profile-dir",
            Layer::Profile => "profile",
            Layer::Launch => "launch",
            Layer::Runtime => "runtime",
        }
    }
}

/// Where a value came from (`apoth config show --origin`).
#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    pub layer: Layer,
    pub file: Option<PathBuf>,
    pub line: usize,
    pub column: usize,
    /// Set under a `when` block.
    pub when: bool,
}

impl Origin {
    pub fn builtin() -> Origin {
        Origin {
            layer: Layer::Default,
            file: None,
            line: 0,
            column: 0,
            when: false,
        }
    }
    pub fn at(layer: Layer) -> Origin {
        Origin {
            layer,
            file: None,
            line: 0,
            column: 0,
            when: false,
        }
    }
    /// `user ~/.config/eclipse/apotheosis.kdl:12`.
    pub fn describe(&self) -> String {
        match &self.file {
            Some(f) if self.line > 0 => {
                format!("{} {}:{}", self.layer.name(), f.display(), self.line)
            }
            Some(f) => format!("{} {}", self.layer.name(), f.display()),
            None => self.layer.name().to_string(),
        }
    }
}

/// A collection entry with the place it was declared.
#[derive(Debug, Clone, PartialEq)]
pub struct Spanned<T> {
    pub value: T,
    pub origin: Origin,
}

/// A scalar argument of an action or layout node.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
}

/// An action invocation as written: `pane.split direction="right"`. Resolution against the
/// `apoth-action` registry (argument typing, capability class) is the integration step; here
/// the id is checked against the closed families and argv placeholders are validated.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionCall {
    pub id: String,
    pub args: Vec<Arg>,
    pub props: Vec<(String, Arg)>,
    pub children: Vec<ActionCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BindDecl {
    pub chords: Vec<Chord>,
    pub actions: Vec<ActionCall>,
    pub repeat: bool,
    pub mode: Option<String>,
    pub release: bool,
    pub global: bool,
    pub performable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MouseBindDecl {
    pub trigger: String,
    pub on: String,
    pub actions: Vec<ActionCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyTableDecl {
    pub name: String,
    pub binds: Vec<BindDecl>,
    /// `unbind` nodes of the table, in source order (a bind of the same chord written
    /// earlier in the block is already gone from `binds`).
    pub unbinds: Vec<Vec<Chord>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MacroDecl {
    pub name: String,
    pub actions: Vec<ActionCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuleDecl {
    pub name: String,
    pub event: String,
    pub when: Option<String>,
    pub actions: Vec<ActionCall>,
    pub allow_input: bool,
    pub allow_spawn: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileMatch {
    pub host: Option<String>,
    pub user: Option<String>,
    pub path: Option<String>,
    pub job: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileDecl {
    pub name: String,
    pub inherits: Option<String>,
    pub matches: Vec<ProfileMatch>,
    pub sticky: bool,
    /// `profilable` keys the profile sets, by dotted path, in file order.
    pub values: Vec<(String, Value)>,
    pub binds: Vec<BindDecl>,
    /// `unbind` nodes of the profile's `binds` block.
    pub unbinds: Vec<Vec<Chord>>,
    /// `security { <class> allow|ask|deny }`.
    pub security: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayoutDecl {
    pub name: String,
    pub body: Vec<ActionCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuDecl {
    pub name: String,
    pub items: Vec<ActionCall>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DomainDecl {
    pub name: String,
    pub remote: Option<bool>,
    pub security: Vec<(String, String)>,
    pub body: Vec<ActionCall>,
}

/// One table key's current value and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    pub value: Value,
    pub origin: Origin,
}

/// The effective configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub(crate) slots: Vec<Slot>,
    pub version: u32,
    pub binds: Vec<Spanned<BindDecl>>,
    pub unbinds: Vec<Spanned<Vec<Chord>>>,
    pub mouse_binds: Vec<Spanned<MouseBindDecl>>,
    pub key_tables: Vec<Spanned<KeyTableDecl>>,
    pub macros: Vec<Spanned<MacroDecl>>,
    pub rules: Vec<Spanned<RuleDecl>>,
    pub profiles: Vec<Spanned<ProfileDecl>>,
    pub layouts: Vec<Spanned<LayoutDecl>>,
    pub menus: Vec<Spanned<MenuDecl>>,
    pub domains: Vec<Spanned<DomainDecl>>,
}

impl Default for Config {
    fn default() -> Self {
        Config::defaults()
    }
}

impl Config {
    /// Layer 1: every key at its default.
    pub fn defaults() -> Config {
        Config {
            slots: TABLE
                .iter()
                .map(|k| Slot {
                    value: k.default_value(),
                    origin: Origin::builtin(),
                })
                .collect(),
            version: 1,
            binds: Vec::new(),
            unbinds: Vec::new(),
            mouse_binds: Vec::new(),
            key_tables: Vec::new(),
            macros: Vec::new(),
            rules: Vec::new(),
            profiles: Vec::new(),
            layouts: Vec::new(),
            menus: Vec::new(),
            domains: Vec::new(),
        }
    }

    pub fn slot(&self, path: &str) -> Option<&Slot> {
        schema::index_of(path).map(|i| &self.slots[i])
    }

    pub fn get(&self, path: &str) -> Option<&Value> {
        self.slot(path).map(|s| &s.value)
    }

    pub fn origin(&self, path: &str) -> Option<&Origin> {
        self.slot(path).map(|s| &s.origin)
    }

    /// Is the key still at its `Computed` default (shown as "Automatic")?
    pub fn is_automatic(&self, path: &str) -> bool {
        match (schema::key(path), self.slot(path)) {
            (Some(k), Some(s)) => k.default.is_computed() && s.origin.layer == Layer::Default,
            _ => false,
        }
    }

    pub fn bool(&self, path: &str) -> bool {
        self.get(path).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn int(&self, path: &str) -> i64 {
        self.get(path).and_then(Value::as_int).unwrap_or(0)
    }
    pub fn float(&self, path: &str) -> f64 {
        self.get(path).and_then(Value::as_float).unwrap_or(0.0)
    }
    /// A string-valued key; `None` when unset.
    pub fn str(&self, path: &str) -> Option<&str> {
        self.get(path).and_then(Value::as_str)
    }
    pub fn list(&self, path: &str) -> &[String] {
        self.get(path).and_then(Value::as_list).unwrap_or(&[])
    }
    pub fn color(&self, path: &str) -> Option<[u8; 4]> {
        self.get(path).and_then(Value::as_color)
    }
    /// Duration keys are stored in milliseconds, byte keys in bytes.
    pub fn millis(&self, path: &str) -> u64 {
        self.int(path).max(0) as u64
    }
    pub fn bytes(&self, path: &str) -> u64 {
        self.int(path).max(0) as u64
    }

    pub(crate) fn set_slot(&mut self, idx: usize, value: Value, origin: Origin) {
        let key: &Key = &TABLE[idx];
        let value = if schema::EXTEND_ONLY.contains(&key.path) {
            // Extend-only lists accumulate over the default instead of replacing it.
            let mut merged: Vec<String> = match &self.slots[idx].value {
                Value::List(l) => l.clone(),
                _ => Vec::new(),
            };
            if let Value::List(new) = &value {
                for n in new {
                    if !merged.contains(n) {
                        merged.push(n.clone());
                    }
                }
            }
            Value::List(merged)
        } else {
            value
        };
        self.slots[idx] = Slot { value, origin };
    }

    /// Set a key by path at the given origin (used by layers 6-8). Unknown path: `false`.
    pub fn set_at(&mut self, path: &str, value: Value, origin: Origin) -> bool {
        match schema::index_of(path) {
            Some(i) => {
                self.set_slot(i, value, origin);
                true
            }
            None => false,
        }
    }

    /// Every key with its value and origin, in table order.
    pub fn entries(&self) -> impl Iterator<Item = (&'static Key, &Slot)> {
        TABLE.iter().zip(self.slots.iter())
    }
}
