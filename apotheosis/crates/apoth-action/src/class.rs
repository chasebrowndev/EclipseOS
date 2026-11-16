// SPDX-License-Identifier: Apache-2.0
//! Capability classes (T-12 §4), config override classes (T-10 §1.1), per-value
//! classification and the rule-consent check (T-11 §5).

use std::fmt;

/// A capability class (T-12 §4). A pair of classes means both are required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    /// tree, info, versions, config read, non-content events.
    Observe,
    /// navigation, close, move, focus, resize, tabs, windows, notify.
    Layout,
    /// theme, font size, appearance-class config.set, reload.
    Appearance,
    /// launch, pipe, open, any argv-carrying Action.
    Spawn,
    /// pane text, blocks, selection.
    ReadContent,
    /// send-text/bytes/keys, paste, broadcast.
    Input,
    /// read/write clipboard through the terminal.
    Clipboard,
    /// key-event stream (config opt-in only).
    Keys,
    /// config.persist, config.save-override, behaviour config.set.
    ConfigWrite,
}

impl Class {
    /// Every class.
    pub const ALL: [Class; 9] = [
        Class::Observe,
        Class::Layout,
        Class::Appearance,
        Class::Spawn,
        Class::ReadContent,
        Class::Input,
        Class::Clipboard,
        Class::Keys,
        Class::ConfigWrite,
    ];

    /// The kebab-case name used in config and on the socket.
    pub const fn name(self) -> &'static str {
        match self {
            Class::Observe => "observe",
            Class::Layout => "layout",
            Class::Appearance => "appearance",
            Class::Spawn => "spawn",
            Class::ReadContent => "read-content",
            Class::Input => "input",
            Class::Clipboard => "clipboard",
            Class::Keys => "keys",
            Class::ConfigWrite => "config-write",
        }
    }

    /// Parse a class name.
    pub fn parse(s: &str) -> Option<Class> {
        Class::ALL.iter().copied().find(|c| c.name() == s)
    }

    const fn bit(self) -> u16 {
        1u16 << (self as u16)
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of classes: "all of these are required".
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ClassSet(u16);

impl ClassSet {
    /// The empty set.
    pub const EMPTY: ClassSet = ClassSet(0);

    /// Build from a slice (const).
    pub const fn of(classes: &[Class]) -> ClassSet {
        let mut bits = 0u16;
        let mut i = 0;
        while i < classes.len() {
            bits |= classes[i].bit();
            i += 1;
        }
        ClassSet(bits)
    }

    /// This set plus `c`.
    #[must_use]
    pub const fn with(self, c: Class) -> ClassSet {
        ClassSet(self.0 | c.bit())
    }

    /// Union.
    #[must_use]
    pub const fn union(self, other: ClassSet) -> ClassSet {
        ClassSet(self.0 | other.0)
    }

    /// Membership.
    pub const fn contains(self, c: Class) -> bool {
        self.0 & c.bit() != 0
    }

    /// True when every member of `self` is in `other`.
    pub const fn is_subset(self, other: ClassSet) -> bool {
        self.0 & !other.0 == 0
    }

    /// True when empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Number of classes.
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Members in declaration order.
    pub fn iter(self) -> impl Iterator<Item = Class> {
        Class::ALL.into_iter().filter(move |c| self.contains(*c))
    }
}

impl fmt::Debug for ClassSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl fmt::Display for ClassSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.iter().map(Class::name).collect();
        f.write_str(&names.join("+"))
    }
}

/// A config key's override class (T-10 §1.1). apoth-config owns the per-key table; this
/// type lives here so `config.set` can be classified without a dependency cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverrideClass {
    /// colours, fonts, opacity, padding, cursor style, looks.
    Appearance,
    /// scrollback size, mouse, selection, bell switches, notification filters.
    Behaviour,
    /// clipboard, paste, links, rule, binds, argv-typed keys, ...
    Security,
    /// everything under `socket`.
    Grant,
}

impl OverrideClass {
    /// The name used in diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            OverrideClass::Appearance => "appearance",
            OverrideClass::Behaviour => "behaviour",
            OverrideClass::Security => "security",
            OverrideClass::Grant => "grant",
        }
    }

    /// Parse; an unknown name is `Grant` (fail-closed, T-10 §1.1).
    pub fn parse_fail_closed(s: &str) -> OverrideClass {
        match s {
            "appearance" => OverrideClass::Appearance,
            "behaviour" => OverrideClass::Behaviour,
            "security" => OverrideClass::Security,
            _ => OverrideClass::Grant,
        }
    }
}

impl fmt::Display for OverrideClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why an Action is refused outright regardless of granted classes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Runtime set/reset of a `security` key (persist needs config-write instead).
    RuntimeSecurityKey,
    /// Any write of a `grant` key over the socket or at runtime.
    GrantKey,
    /// `macro.run` of a name the context does not know.
    UnknownMacro,
    /// Macro expansion nested deeper than the cap.
    MacroDepth,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::RuntimeSecurityKey => {
                f.write_str("key has override class `security`: not settable at runtime")
            }
            Refusal::GrantKey => {
                f.write_str("key has override class `grant`: never writable over the socket")
            }
            Refusal::UnknownMacro => f.write_str("unknown macro"),
            Refusal::MacroDepth => f.write_str("macro nesting too deep"),
        }
    }
}

/// The capability requirement of one Action value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Classification {
    /// All of these classes are required.
    pub classes: ClassSet,
    /// When set, the Action is refused whatever the grants.
    pub refused: Option<Refusal>,
}

impl Classification {
    /// A classification with a fixed set.
    pub fn of(classes: &[Class]) -> Self {
        Self {
            classes: ClassSet::of(classes),
            refused: None,
        }
    }

    /// Add one class.
    pub fn add(&mut self, c: Class) {
        self.classes = self.classes.with(c);
    }

    /// Refuse (first refusal wins).
    pub fn refuse(&mut self, r: Refusal) {
        if self.refused.is_none() {
            self.refused = Some(r);
        }
    }

    /// Union with another classification.
    pub fn merge(&mut self, other: Classification) {
        self.classes = self.classes.union(other.classes);
        if let Some(r) = other.refused {
            self.refuse(r);
        }
    }
}

/// Facts the class function needs from outside this crate. Every method defaults to
/// "unknown", which classifies as strictly as possible (fail-closed).
pub trait ClassContext {
    /// Override class of a config key; `None` = unknown key (treated as `grant`).
    fn override_class(&self, _key: &str) -> Option<OverrideClass> {
        None
    }
    /// Members of a macro; `None` = unknown (refused).
    fn macro_members(&self, _name: &str) -> Option<&[crate::Action]> {
        None
    }
    /// Whether a layout carries any argv; `None` = unknown (treated as yes).
    fn layout_has_argv(&self, _name: &str) -> Option<bool> {
        None
    }
}

/// A context that knows nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoContext;

impl ClassContext for NoContext {}

/// Why a rule's action is not allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentError {
    /// Class never allowed from a rule: `read-content`, `config-write`, `keys`.
    Never(Class),
    /// `input` or `clipboard` without `allow-input`.
    NeedsAllowInput(Class),
    /// `spawn` without `allow-spawn`.
    NeedsAllowSpawn,
    /// The Action is refused outright.
    Refused(Refusal),
}

impl fmt::Display for ConsentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConsentError::Never(c) => write!(f, "rules may never run `{c}` actions"),
            ConsentError::NeedsAllowInput(c) => {
                write!(f, "`{c}` actions need allow-input=#true on the rule")
            }
            ConsentError::NeedsAllowSpawn => f.write_str("spawn actions need allow-spawn=#true"),
            ConsentError::Refused(r) => write!(f, "{r}"),
        }
    }
}

/// Rule consent by capability class (T-11 §5), applied after macro expansion.
/// Applied regardless of the event's trust: it is the stricter reading and the load-time
/// check cannot know the cause of a future event.
pub fn rule_consent(
    c: &Classification,
    allow_input: bool,
    allow_spawn: bool,
) -> Result<(), ConsentError> {
    if let Some(r) = &c.refused {
        return Err(ConsentError::Refused(r.clone()));
    }
    for class in [Class::ReadContent, Class::ConfigWrite, Class::Keys] {
        if c.classes.contains(class) {
            return Err(ConsentError::Never(class));
        }
    }
    for class in [Class::Input, Class::Clipboard] {
        if c.classes.contains(class) && !allow_input {
            return Err(ConsentError::NeedsAllowInput(class));
        }
    }
    if c.classes.contains(Class::Spawn) && !allow_spawn {
        return Err(ConsentError::NeedsAllowSpawn);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_ops() {
        let s = ClassSet::of(&[Class::Spawn, Class::ReadContent]);
        assert!(s.contains(Class::Spawn));
        assert!(!s.contains(Class::Input));
        assert_eq!(s.len(), 2);
        assert!(ClassSet::of(&[Class::Spawn]).is_subset(s));
        assert!(!s.is_subset(ClassSet::of(&[Class::Spawn])));
        assert_eq!(s.to_string(), "spawn+read-content");
        assert!(ClassSet::EMPTY.is_empty());
        assert_eq!(s.union(ClassSet::of(&[Class::Input])).len(), 3);
    }

    #[test]
    fn class_names_roundtrip() {
        for c in Class::ALL {
            assert_eq!(Class::parse(c.name()), Some(c));
        }
        assert_eq!(Class::parse("bogus"), None);
    }

    #[test]
    fn unknown_override_class_is_grant() {
        assert_eq!(
            OverrideClass::parse_fail_closed("???"),
            OverrideClass::Grant
        );
        assert_eq!(
            OverrideClass::parse_fail_closed("appearance"),
            OverrideClass::Appearance
        );
    }

    #[test]
    fn consent_by_class() {
        let ok = Classification::of(&[Class::Observe, Class::Layout, Class::Appearance]);
        assert_eq!(rule_consent(&ok, false, false), Ok(()));
        let inp = Classification::of(&[Class::Input]);
        assert_eq!(
            rule_consent(&inp, false, true),
            Err(ConsentError::NeedsAllowInput(Class::Input))
        );
        assert_eq!(rule_consent(&inp, true, false), Ok(()));
        let cb = Classification::of(&[Class::Clipboard]);
        assert!(rule_consent(&cb, false, false).is_err());
        let sp = Classification::of(&[Class::Spawn]);
        assert_eq!(
            rule_consent(&sp, true, false),
            Err(ConsentError::NeedsAllowSpawn)
        );
        assert_eq!(rule_consent(&sp, false, true), Ok(()));
        for c in [Class::ReadContent, Class::ConfigWrite, Class::Keys] {
            let k = Classification::of(&[c]);
            assert_eq!(rule_consent(&k, true, true), Err(ConsentError::Never(c)));
        }
        let mut r = Classification::default();
        r.refuse(Refusal::GrantKey);
        assert!(matches!(
            rule_consent(&r, true, true),
            Err(ConsentError::Refused(Refusal::GrantKey))
        ));
    }
}
