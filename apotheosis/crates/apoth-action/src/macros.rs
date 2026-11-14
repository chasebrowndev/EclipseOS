// SPDX-License-Identifier: Apache-2.0
//! Macros (T-11 §4): `macro "name" { action; action; … }`.
//!
//! A macro set is checked as a whole at load: names are unique, every `macro.run`
//! reference resolves, and there is no recursion (direct or through other macros).
//! References may point forward; the set is built from all definitions at once. The
//! class of a macro is the union of its members' classes after expansion; it is computed
//! by [`crate::Action::classify`] through a [`WithMacros`] context and must be computed
//! again at dispatch (config or context may have changed).

use std::collections::HashMap;
use std::fmt;

use crate::action::{Action, MAX_NAME_BYTES};
use crate::class::{ClassContext, OverrideClass};

/// Most members in one macro.
pub const MAX_MACRO_MEMBERS: usize = 256;

/// Most macros in one set.
pub const MAX_MACROS: usize = 1024;

/// One macro definition.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroDef {
    /// Name.
    pub name: String,
    /// Ordered members.
    pub members: Vec<Action>,
}

/// Why a macro set was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacroError {
    /// Empty, too long or containing control characters.
    BadName(String),
    /// Defined twice.
    Duplicate(String),
    /// Too many members.
    TooManyMembers(String),
    /// More than [`MAX_MACROS`] definitions.
    TooManyMacros,
    /// `macro.run` of a name that is not defined.
    Unknown {
        /// The macro containing the reference.
        in_macro: String,
        /// The missing name.
        reference: String,
    },
    /// A cycle; the path starts and ends at the same name.
    Recursive(Vec<String>),
}

impl fmt::Display for MacroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MacroError::BadName(n) => write!(f, "bad macro name `{n}`"),
            MacroError::Duplicate(n) => write!(f, "macro `{n}` is defined twice"),
            MacroError::TooManyMembers(n) => write!(f, "macro `{n}` has too many actions"),
            MacroError::TooManyMacros => write!(f, "more than {MAX_MACROS} macros"),
            MacroError::Unknown {
                in_macro,
                reference,
            } => write!(f, "macro `{in_macro}` runs unknown macro `{reference}`"),
            MacroError::Recursive(p) => write!(f, "macros may not recurse: {}", p.join(" -> ")),
        }
    }
}

impl std::error::Error for MacroError {}

/// A validated set of macros.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MacroSet {
    defs: Vec<MacroDef>,
    index: HashMap<String, usize>,
}

impl MacroSet {
    /// Check and build a set from all definitions at once.
    pub fn build(defs: Vec<MacroDef>) -> Result<MacroSet, MacroError> {
        if defs.len() > MAX_MACROS {
            return Err(MacroError::TooManyMacros);
        }
        let mut index = HashMap::with_capacity(defs.len());
        for (i, d) in defs.iter().enumerate() {
            if d.name.is_empty()
                || d.name.len() > MAX_NAME_BYTES
                || d.name.chars().any(char::is_control)
            {
                return Err(MacroError::BadName(d.name.clone()));
            }
            if d.members.len() > MAX_MACRO_MEMBERS {
                return Err(MacroError::TooManyMembers(d.name.clone()));
            }
            if index.insert(d.name.clone(), i).is_some() {
                return Err(MacroError::Duplicate(d.name.clone()));
            }
        }
        let set = MacroSet { defs, index };
        let mut edges: Vec<Vec<usize>> = Vec::with_capacity(set.defs.len());
        for d in &set.defs {
            let mut out = Vec::new();
            for m in &d.members {
                if let Action::MacroRun { name } = m {
                    match set.index.get(name) {
                        Some(j) => out.push(*j),
                        None => {
                            return Err(MacroError::Unknown {
                                in_macro: d.name.clone(),
                                reference: name.clone(),
                            });
                        }
                    }
                }
            }
            edges.push(out);
        }
        // Depth-first search; 0 = unvisited, 1 = on the stack, 2 = done.
        let mut state = vec![0u8; set.defs.len()];
        let mut stack: Vec<usize> = Vec::new();
        for start in 0..set.defs.len() {
            if let Some(cycle) = set.visit(start, &edges, &mut state, &mut stack) {
                return Err(MacroError::Recursive(cycle));
            }
        }
        Ok(set)
    }

    /// Iterative depth-first search (no recursion: a long chain cannot overflow the stack).
    fn visit(
        &self,
        start: usize,
        edges: &[Vec<usize>],
        state: &mut [u8],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<String>> {
        if state[start] != 0 {
            return None;
        }
        // Frames of (node, next edge index); `stack` mirrors the nodes.
        let mut frames: Vec<(usize, usize)> = vec![(start, 0)];
        state[start] = 1;
        stack.clear();
        stack.push(start);
        while let Some(frame) = frames.last_mut() {
            let n = frame.0;
            if frame.1 < edges[n].len() {
                let next = edges[n][frame.1];
                frame.1 += 1;
                match state[next] {
                    2 => {}
                    1 => {
                        let at = stack.iter().position(|x| *x == next).unwrap_or(0);
                        let mut path: Vec<String> = stack[at..]
                            .iter()
                            .map(|i| self.defs[*i].name.clone())
                            .collect();
                        path.push(self.defs[next].name.clone());
                        return Some(path);
                    }
                    _ => {
                        state[next] = 1;
                        stack.push(next);
                        frames.push((next, 0));
                    }
                }
            } else {
                state[n] = 2;
                stack.pop();
                frames.pop();
            }
        }
        None
    }

    /// A macro by name.
    pub fn get(&self, name: &str) -> Option<&MacroDef> {
        self.index.get(name).map(|i| &self.defs[*i])
    }

    /// Number of macros.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// A classification context that knows these macros and defers everything else.
    pub fn with<'a>(&'a self, inner: &'a dyn ClassContext) -> WithMacros<'a> {
        WithMacros { set: self, inner }
    }
}

/// A [`ClassContext`] layering a [`MacroSet`] over another context.
pub struct WithMacros<'a> {
    set: &'a MacroSet,
    inner: &'a dyn ClassContext,
}

impl fmt::Debug for WithMacros<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WithMacros")
            .field("macros", &self.set.len())
            .finish()
    }
}

impl ClassContext for WithMacros<'_> {
    fn override_class(&self, key: &str) -> Option<OverrideClass> {
        self.inner.override_class(key)
    }
    fn macro_members(&self, name: &str) -> Option<&[Action]> {
        self.set.get(name).map(|d| d.members.as_slice())
    }
    fn layout_has_argv(&self, name: &str) -> Option<bool> {
        self.inner.layout_has_argv(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::{Class, ClassSet, NoContext, Refusal};

    fn a(s: &str) -> Action {
        Action::parse(s).unwrap()
    }

    fn def(name: &str, members: &[&str]) -> MacroDef {
        MacroDef {
            name: name.into(),
            members: members.iter().map(|m| a(m)).collect(),
        }
    }

    #[test]
    fn rejects_recursion_direct_indirect_and_self() {
        let e = MacroSet::build(vec![def("a", &["macro.run a"])]).unwrap_err();
        assert_eq!(e, MacroError::Recursive(vec!["a".into(), "a".into()]));
        let e = MacroSet::build(vec![
            def("a", &["macro.run b"]),
            def("b", &["tab.next", "macro.run c"]),
            def("c", &["macro.run a"]),
        ])
        .unwrap_err();
        assert!(matches!(e, MacroError::Recursive(p) if p.len() == 4 && p[0] == p[3]));
    }

    #[test]
    fn forward_references_and_diamonds_are_fine() {
        let s = MacroSet::build(vec![
            def("top", &["macro.run left", "macro.run right"]),
            def("left", &["macro.run leaf"]),
            def("right", &["macro.run leaf"]),
            def("leaf", &["tab.next"]),
        ])
        .unwrap();
        assert_eq!(s.len(), 4);
        assert!(s.get("leaf").is_some());
    }

    #[test]
    fn rejects_unknown_duplicate_and_bad_names() {
        assert!(matches!(
            MacroSet::build(vec![def("a", &["macro.run zz"])]),
            Err(MacroError::Unknown { .. })
        ));
        assert!(matches!(
            MacroSet::build(vec![def("a", &[]), def("a", &[])]),
            Err(MacroError::Duplicate(_))
        ));
        assert!(matches!(
            MacroSet::build(vec![def("", &[])]),
            Err(MacroError::BadName(_))
        ));
    }

    #[test]
    fn class_is_union_of_members_after_expansion() {
        let s = MacroSet::build(vec![
            def("outer", &["tab.next", "macro.run inner"]),
            def("inner", &["input.send-text hi", "pane.split right -- htop"]),
        ])
        .unwrap();
        let ctx = s.with(&NoContext);
        let c = a("macro.run outer").classify(&ctx);
        assert_eq!(
            c.classes,
            ClassSet::of(&[Class::Layout, Class::Input, Class::Spawn])
        );
        assert_eq!(c.refused, None);
    }

    #[test]
    fn argument_conditional_member_contributes_strictest() {
        let s = MacroSet::build(vec![
            def("plain", &["pane.split right"]),
            def("cmd", &["pane.split right -- htop"]),
            def("capt", &["capture visible command -- cat"]),
            def("lay", &["layout.apply dev"]),
        ])
        .unwrap();
        let ctx = s.with(&NoContext);
        assert!(
            !a("macro.run plain")
                .classify(&ctx)
                .classes
                .contains(Class::Spawn)
        );
        assert!(
            a("macro.run cmd")
                .classify(&ctx)
                .classes
                .contains(Class::Spawn)
        );
        let capt = a("macro.run capt").classify(&ctx).classes;
        assert!(capt.contains(Class::Spawn) && capt.contains(Class::ReadContent));
        // unknown layout: assume argv
        assert!(
            a("macro.run lay")
                .classify(&ctx)
                .classes
                .contains(Class::Spawn)
        );
    }

    #[test]
    fn unknown_macro_at_dispatch_is_refused() {
        let c = a("macro.run ghost").classify(&NoContext);
        assert_eq!(c.refused, Some(Refusal::UnknownMacro));
    }

    #[test]
    fn dispatch_time_recompute_sees_changed_set() {
        let v1 = MacroSet::build(vec![def("m", &["tab.next"])]).unwrap();
        let v2 = MacroSet::build(vec![def("m", &["launch tab -- sh"])]).unwrap();
        let act = a("macro.run m");
        assert!(
            !act.classify(&v1.with(&NoContext))
                .classes
                .contains(Class::Spawn)
        );
        assert!(
            act.classify(&v2.with(&NoContext))
                .classes
                .contains(Class::Spawn)
        );
    }

    #[test]
    fn dag_of_wide_macros_classifies_fast() {
        let mut defs = Vec::new();
        for i in 0..9 {
            let members = if i == 8 {
                vec![]
            } else {
                (0..256)
                    .map(|_| Action::MacroRun {
                        name: format!("m{}", i + 1),
                    })
                    .collect()
            };
            defs.push(MacroDef {
                name: format!("m{i}"),
                members,
            });
        }
        let set = MacroSet::build(defs).unwrap();
        let run = Action::MacroRun { name: "m0".into() };
        let _ = run.classify(&set.with(&crate::class::NoContext));
    }

    #[test]
    fn long_chain_does_not_overflow_and_count_is_capped() {
        let n = MAX_MACROS;
        let defs: Vec<MacroDef> = (0..n)
            .map(|i| MacroDef {
                name: format!("m{i}"),
                members: if i + 1 < n {
                    vec![Action::MacroRun {
                        name: format!("m{}", i + 1),
                    }]
                } else {
                    vec![]
                },
            })
            .collect();
        assert!(MacroSet::build(defs).is_ok());
        let too_many: Vec<MacroDef> = (0..=MAX_MACROS)
            .map(|i| MacroDef {
                name: format!("m{i}"),
                members: vec![],
            })
            .collect();
        assert_eq!(MacroSet::build(too_many), Err(MacroError::TooManyMacros));
    }
}
