// SPDX-License-Identifier: Apache-2.0
//! The method table (T-12 §2) with each method's capability class (T-12 §4), exposed as
//! data for the capability matrix test and the server's gate.
//!
//! Methods whose class depends on the argument (`action.run`, `config.set`,
//! `config.reset`) have `dynamic: true` and an empty static set; use
//! [`crate::Params::classify`] for those.

use apoth_action::{Class, ClassSet};

/// Every control-socket method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Hello,
    ActionRun,
    InputSendKeys,
    Tree,
    PaneInfo,
    PaneProcesses,
    PaneText,
    PaneBlocks,
    ConfigGet,
    ConfigEffective,
    KeyboardState,
    GraphicsList,
    MediaList,
    WaylandInfo,
    RenderStatus,
    Version,
    Subscribe,
    Unsubscribe,
    PaneWatch,
    ConfigSet,
    ConfigReset,
    ConfigReload,
    ConfigPersist,
    ConfigSaveOverride,
    Notify,
    Launch,
}

/// One row of the method table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodInfo {
    /// The method.
    pub method: Method,
    /// Wire name.
    pub name: &'static str,
    /// Classes, all required. Empty for `hello` and for `dynamic` methods.
    pub classes: ClassSet,
    /// Class depends on the arguments (computed from the Action).
    pub dynamic: bool,
    /// Returns content-bearing fields that are `null` to agent-descendant peers without
    /// `read-content` and to every peer in the credential state (T-12 §4 `observe`).
    pub redacts_content: bool,
    /// Reserved: R2 only (the server may answer `unsupported`).
    pub r2: bool,
    /// Takes a `target` match expression (default `self`).
    pub targeted: bool,
}

const fn row(
    method: Method,
    name: &'static str,
    classes: &[Class],
    dynamic: bool,
    redacts_content: bool,
    r2: bool,
    targeted: bool,
) -> MethodInfo {
    MethodInfo {
        method,
        name,
        classes: ClassSet::of(classes),
        dynamic,
        redacts_content,
        r2,
        targeted,
    }
}

use Class::{ConfigWrite, Input, Layout, Observe, ReadContent, Spawn};

/// The method table: one row per [`Method`], in declaration order.
pub const METHODS: &[MethodInfo] = &[
    row(Method::Hello, "hello", &[], false, false, false, false),
    row(
        Method::ActionRun,
        "action.run",
        &[],
        true,
        false,
        false,
        true,
    ),
    row(
        Method::InputSendKeys,
        "input.send-keys",
        &[Input],
        false,
        false,
        false,
        true,
    ),
    row(Method::Tree, "tree", &[Observe], false, true, false, true),
    row(
        Method::PaneInfo,
        "pane.info",
        &[Observe],
        false,
        true,
        false,
        true,
    ),
    row(
        Method::PaneProcesses,
        "pane.processes",
        &[Observe],
        false,
        true,
        false,
        true,
    ),
    row(
        Method::PaneText,
        "pane.text",
        &[ReadContent],
        false,
        false,
        false,
        true,
    ),
    row(
        Method::PaneBlocks,
        "pane.blocks",
        &[ReadContent],
        false,
        false,
        false,
        true,
    ),
    row(
        Method::ConfigGet,
        "config.get",
        &[Observe],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::ConfigEffective,
        "config.effective",
        &[Observe],
        false,
        true,
        false,
        true,
    ),
    row(
        Method::KeyboardState,
        "keyboard.state",
        &[Observe],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::GraphicsList,
        "graphics.list",
        &[Observe],
        false,
        false,
        false,
        true,
    ),
    row(
        Method::MediaList,
        "media.list",
        &[Observe],
        false,
        false,
        false,
        true,
    ),
    row(
        Method::WaylandInfo,
        "wayland.info",
        &[Observe],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::RenderStatus,
        "render.status",
        &[Observe],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::Version,
        "version",
        &[Observe],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::Subscribe,
        "subscribe",
        &[Observe],
        false,
        true,
        false,
        false,
    ),
    row(
        Method::Unsubscribe,
        "unsubscribe",
        &[Observe],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::PaneWatch,
        "pane.watch",
        &[ReadContent],
        false,
        false,
        true,
        true,
    ),
    row(
        Method::ConfigSet,
        "config.set",
        &[],
        true,
        false,
        false,
        false,
    ),
    row(
        Method::ConfigReset,
        "config.reset",
        &[],
        true,
        false,
        false,
        false,
    ),
    row(
        Method::ConfigReload,
        "config.reload",
        &[Class::Appearance],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::ConfigPersist,
        "config.persist",
        &[ConfigWrite],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::ConfigSaveOverride,
        "config.save-override",
        &[ConfigWrite],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::Notify,
        "notify",
        &[Layout],
        false,
        false,
        false,
        false,
    ),
    row(
        Method::Launch,
        "launch",
        &[Spawn],
        false,
        false,
        false,
        false,
    ),
];

impl Method {
    /// The table row.
    pub fn info(self) -> &'static MethodInfo {
        METHODS
            .iter()
            .find(|r| r.method == self)
            .expect("every Method has a row (checked by test)")
    }

    /// Wire name.
    pub fn name(self) -> &'static str {
        self.info().name
    }

    /// Parse a wire name.
    pub fn parse(name: &str) -> Option<Method> {
        METHODS.iter().find(|r| r.name == name).map(|r| r.method)
    }

    /// Static classes (empty for `hello` and dynamic methods).
    pub fn classes(self) -> ClassSet {
        self.info().classes
    }

    /// The input-injecting marker: the method needs class `input`, so the server gates it
    /// per T-12 §4 (ask unless the target is the client's own pane; `seat.text` /
    /// `seat.key` for agent peers). For `action.run` use [`injects_input`] on the
    /// computed classification instead.
    pub fn injects_input(self) -> bool {
        self.classes().contains(Input)
    }
}

/// True when a computed class set injects input.
pub fn injects_input(classes: ClassSet) -> bool {
    classes.contains(Input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_one_row_per_method_and_unique_names() {
        let mut names: Vec<_> = METHODS.iter().map(|r| r.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), METHODS.len());
        assert_eq!(METHODS.len(), 26);
        for r in METHODS {
            assert_eq!(Method::parse(r.name), Some(r.method));
            assert_eq!(r.method.info().name, r.name);
        }
    }

    #[test]
    fn spec_methods_are_present() {
        for n in [
            "action.run",
            "input.send-keys",
            "tree",
            "pane.info",
            "pane.processes",
            "pane.text",
            "pane.blocks",
            "config.get",
            "config.effective",
            "keyboard.state",
            "graphics.list",
            "media.list",
            "wayland.info",
            "render.status",
            "version",
            "subscribe",
            "unsubscribe",
            "pane.watch",
            "hello",
            "launch",
            "notify",
            "config.set",
            "config.reset",
            "config.reload",
            "config.persist",
            "config.save-override",
        ] {
            assert!(Method::parse(n).is_some(), "{n}");
        }
    }

    #[test]
    fn no_consent_or_sheet_method_exists() {
        for n in [
            "consent.answer",
            "consent.allow",
            "sheet.answer",
            "paste-guard.confirm",
            "askpass.submit",
            "allow-once",
            "allow-always",
            "deny",
        ] {
            assert!(Method::parse(n).is_none(), "{n}");
        }
    }

    #[test]
    fn classes_per_t12_4() {
        assert!(Method::InputSendKeys.injects_input());
        assert!(!Method::PaneText.injects_input());
        assert!(Method::PaneText.classes().contains(ReadContent));
        assert!(Method::PaneBlocks.classes().contains(ReadContent));
        assert!(Method::PaneWatch.classes().contains(ReadContent));
        assert!(Method::Tree.classes().contains(Observe));
        assert!(Method::Launch.classes().contains(Spawn));
        assert!(Method::ConfigPersist.classes().contains(ConfigWrite));
        assert!(Method::ConfigSaveOverride.classes().contains(ConfigWrite));
        assert!(Method::Hello.classes().is_empty());
        // Nothing statically needs `keys`, and `clipboard` only through Actions.
        assert!(METHODS.iter().all(|r| !r.classes.contains(Class::Keys)));
        // Dynamic methods carry no static set.
        for r in METHODS.iter().filter(|r| r.dynamic) {
            assert!(r.classes.is_empty());
        }
    }
}
