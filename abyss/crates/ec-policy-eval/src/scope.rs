// SPDX-License-Identifier: AGPL-3.0-only
//! S-01 §3 scopes, and the `scene.list` visibility rule built from them.
//!
//! A scope restricts which targets a capability line reaches. One line may
//! carry several scopes and they combine with AND; separate lines (in one
//! grant or across a principal's live grants) combine with OR, and a scope on
//! one line never loosens another (S-01 §4).
//!
//! Visibility is the rule everything else rests on: `scene.list` scope
//! defines what exists for an agent, and nothing outside it exists at all
//! (S-01 §3, COMP-08 §3). [`SceneView::visible`] is that rule, written once.
//! abyss calls it at its one choke point; it never re-derives it.
//!
//! Fail-closed choices this module makes, for the owner's review:
//! - A scope string that does not parse makes its whole line match nothing.
//!   A line is authority, and a clause we cannot read may be the one that
//!   narrows it.
//! - A `scene.list` line with no scopes matches nothing. S-01 §3 calls an
//!   unscoped capability "all targets the agent can otherwise see", but for
//!   `scene.list` that is the visibility rule itself, and S-01 §8 open item 2
//!   leaves unscoped `scene.list` undecided. Until it is decided, it is denied.
//! - A line without a `class:` scope is capped at `public`, the S-01 §2.1
//!   default. `class:<c>` is a ceiling: `class:private` reaches `public` and
//!   `private` windows.
//! - `url:`, `path:`, `host:` and `node_role:` describe things inside a
//!   window, not the window. On a `scene.list` line they match no window.
//!
//! Compilation (parsing, regex building) allocates and happens once, when a
//! grant is admitted. [`SceneView::visible`] does not allocate.

use crate::grant::Grant;
use regex::Regex;

/// Data sensitivity class (C-00). Ordered: `Public < Private < Secret`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Public,
    Private,
    Secret,
}

/// `workspace:<id|own|human>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceSel {
    Id(u64),
    /// An agent workspace owned by this principal.
    Own,
    /// Any human workspace.
    Human,
}

/// `output:<id|virtual|physical>`. An id matches the output's number or its
/// connector name (`output:DP-1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputSel {
    Id(String),
    Virtual,
    Physical,
}

/// One parsed scope clause.
#[derive(Debug, Clone)]
pub enum Scope {
    Workspace(WorkspaceSel),
    Output(OutputSel),
    AppId(Glob),
    Title(Regex),
    Handle(u64),
    /// `principal:launched_by_self` is `true`; `principal:any` is `false`.
    LaunchedBySelf(bool),
    Class(Class),
    /// `url:`, `path:`, `host:`, `node_role:`: scopes over a window's
    /// contents. Parsed so a grant naming them is well-formed; never a match
    /// on a window.
    Content,
}

/// Why a scope string was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeError {
    /// No `kind:value` separator, or an empty value.
    Malformed,
    /// A kind S-01 §3 does not define.
    UnknownKind,
    /// A known kind with a value outside its grammar.
    BadValue,
}

impl Scope {
    /// Parses one `kind:value` clause (S-01 §3).
    pub fn parse(s: &str) -> Result<Scope, ScopeError> {
        let (kind, value) = s.split_once(':').ok_or(ScopeError::Malformed)?;
        if value.is_empty() {
            return Err(ScopeError::Malformed);
        }
        Ok(match kind {
            "workspace" => Scope::Workspace(match value {
                "own" => WorkspaceSel::Own,
                "human" => WorkspaceSel::Human,
                v => WorkspaceSel::Id(v.parse().map_err(|_| ScopeError::BadValue)?),
            }),
            "output" => Scope::Output(match value {
                "virtual" => OutputSel::Virtual,
                "physical" => OutputSel::Physical,
                v => OutputSel::Id(v.to_owned()),
            }),
            "app_id" => Scope::AppId(Glob::new(value)),
            // Anchored: a title scope names the whole title, not a substring
            // of it, so `title:Inbox` does not reach "Inbox — evil.example".
            "title" => Scope::Title(Regex::new(&format!("^(?:{value})$")).map_err(|_| ScopeError::BadValue)?),
            "handle" => Scope::Handle(value.parse().map_err(|_| ScopeError::BadValue)?),
            "principal" => Scope::LaunchedBySelf(match value {
                "launched_by_self" => true,
                "any" => false,
                _ => return Err(ScopeError::BadValue),
            }),
            "class" => Scope::Class(match value {
                "public" => Class::Public,
                "private" => Class::Private,
                "secret" => Class::Secret,
                _ => return Err(ScopeError::BadValue),
            }),
            "url" | "path" | "host" | "node_role" => Scope::Content,
            _ => return Err(ScopeError::UnknownKind),
        })
    }
}

/// A `*`/`?` glob, matched without allocating. No character classes: S-01
/// §3 does not define any, and an undefined metacharacter is a literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob(String);

impl Glob {
    pub fn new(pattern: &str) -> Glob {
        Glob(pattern.to_owned())
    }

    /// The pattern as written, for the table's wire form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, s: &str) -> bool {
        let p = self.0.as_bytes();
        let s = s.as_bytes();
        // Iterative wildcard match with single-star backtracking: linear in
        // practice, never exponential, and no recursion on attacker input.
        let (mut pi, mut si) = (0, 0);
        let (mut star, mut mark) = (None, 0);
        while si < s.len() {
            if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
                pi += 1;
                si += 1;
            } else if pi < p.len() && p[pi] == b'*' {
                star = Some(pi);
                mark = si;
                pi += 1;
            } else if let Some(sp) = star {
                pi = sp + 1;
                mark += 1;
                si = mark;
            } else {
                return false;
            }
        }
        while pi < p.len() && p[pi] == b'*' {
            pi += 1;
        }
        pi == p.len()
    }
}

/// Where a window sits, as a scope sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceFacts {
    pub id: u64,
    /// A human workspace (C-00 §3.1). Otherwise an agent workspace.
    pub human: bool,
    /// An agent workspace owned by the principal asking.
    pub own: bool,
}

/// The output a window is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFacts<'a> {
    pub id: u64,
    pub name: &'a str,
    pub is_virtual: bool,
}

/// Everything a `scene.list` scope may ask about one window, borrowed from
/// the compositor's own state. Built fresh per check, never cached, so a
/// title change or a class raise is seen by the very next request.
#[derive(Debug, Clone, Copy)]
pub struct WindowFacts<'a> {
    pub handle: u64,
    pub app_id: &'a str,
    pub title: &'a str,
    pub workspace: Option<WorkspaceFacts>,
    pub output: Option<OutputFacts<'a>>,
    pub class: Class,
    /// Launched by the asking principal (COMP-08 `launched_by_self`).
    pub launched_by_self: bool,
    /// Marked `no-agent` by policy (COMP-05 §4). Absent for every agent.
    pub no_agent: bool,
}

impl Scope {
    fn matches(&self, w: &WindowFacts<'_>) -> bool {
        match self {
            Scope::Workspace(sel) => match (sel, w.workspace) {
                (_, None) => false,
                (WorkspaceSel::Id(id), Some(ws)) => ws.id == *id,
                (WorkspaceSel::Own, Some(ws)) => !ws.human && ws.own,
                (WorkspaceSel::Human, Some(ws)) => ws.human,
            },
            Scope::Output(sel) => match (sel, w.output) {
                (_, None) => false,
                (OutputSel::Virtual, Some(o)) => o.is_virtual,
                (OutputSel::Physical, Some(o)) => !o.is_virtual,
                (OutputSel::Id(id), Some(o)) => o.name == id || id.parse::<u64>().is_ok_and(|n| n == o.id),
            },
            Scope::AppId(g) => g.matches(w.app_id),
            Scope::Title(re) => re.is_match(w.title),
            Scope::Handle(h) => w.handle == *h,
            Scope::LaunchedBySelf(required) => !required || w.launched_by_self,
            // Handled as the line's ceiling in `Line::reaches`.
            Scope::Class(_) => true,
            Scope::Content => false,
        }
    }
}

/// One `scene.list` capability line, compiled.
#[derive(Debug, Clone)]
struct Line {
    scopes: Vec<Scope>,
    ceiling: Class,
}

impl Line {
    /// Compiles one line, or `None` if it can reach nothing (see the module
    /// doc for each fail-closed case).
    fn compile(scopes: &[String]) -> Option<Line> {
        if scopes.is_empty() {
            return None;
        }
        let scopes = scopes
            .iter()
            .map(|s| Scope::parse(s))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        // Several `class:` clauses AND together, so the tightest wins.
        let ceiling = scopes
            .iter()
            .filter_map(|s| match s {
                Scope::Class(c) => Some(*c),
                _ => None,
            })
            .min()
            .unwrap_or(Class::Public);
        Some(Line { scopes, ceiling })
    }

    fn reaches(&self, w: &WindowFacts<'_>) -> bool {
        w.class <= self.ceiling && self.scopes.iter().all(|s| s.matches(w))
    }
}

/// The compiled visibility of one principal: every `scene.list` line of
/// every live grant it holds.
#[derive(Debug, Clone, Default)]
pub struct SceneView {
    lines: Vec<Line>,
}

/// The capability whose scope defines what exists for an agent.
pub const SCENE_LIST: &str = "scene.list";

/// The capability whose scope defines which existing windows an agent may
/// read in detail (`get_toplevel`, `hit_test`; S-01 §2.1).
pub const SCENE_READ: &str = "scene.read";

impl SceneView {
    /// Builds the view from a principal's live, verified grants. Callers pass
    /// only grants that came out of [`Grant::verify`] and are not revoked;
    /// this function does not re-check either.
    pub fn compile<'a>(grants: impl IntoIterator<Item = &'a Grant>) -> SceneView {
        SceneView::compile_for(SCENE_LIST, grants)
    }

    /// The same compilation over another capability's scopes, under the
    /// same rules (an unscoped line matches nothing, the class ceiling
    /// defaults to `public`).
    pub fn compile_for<'a>(cap: &str, grants: impl IntoIterator<Item = &'a Grant>) -> SceneView {
        let lines = grants
            .into_iter()
            .flat_map(|g| g.capabilities.iter())
            .filter(|c| c.name == cap)
            .filter_map(|c| Line::compile(&c.scopes))
            .collect();
        SceneView { lines }
    }

    /// Whether `w` exists for this principal. The one visibility rule:
    /// listings, hit tests, events, captures and `wait_for` all ask this and
    /// nothing else (COMP-15 §2).
    pub fn visible(&self, w: &WindowFacts<'_>) -> bool {
        !w.no_agent && self.lines.iter().any(|l| l.reaches(w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant::{Capability, Constraints};
    use crate::task::Ulid;

    fn grant(lines: &[&[&str]]) -> Grant {
        Grant {
            id: Ulid([1; 16]),
            principal: "agent:test".into(),
            issued_ms: 0,
            expires_ms: u64::MAX,
            issuer: "policyd".into(),
            task_id: Ulid([2; 16]),
            capabilities: lines
                .iter()
                .map(|l| Capability {
                    name: SCENE_LIST.into(),
                    scopes: l.iter().map(|s| s.to_string()).collect(),
                    quota: None,
                })
                .collect(),
            constraints: Constraints::default(),
            unattended: false,
        }
    }

    fn view(lines: &[&[&str]]) -> SceneView {
        SceneView::compile([&grant(lines)])
    }

    fn win() -> WindowFacts<'static> {
        WindowFacts {
            handle: 7,
            app_id: "org.mozilla.firefox",
            title: "Invoices — Mozilla Firefox",
            workspace: Some(WorkspaceFacts {
                id: 3,
                human: false,
                own: true,
            }),
            output: Some(OutputFacts {
                id: 1,
                name: "DP-1",
                is_virtual: false,
            }),
            class: Class::Public,
            launched_by_self: true,
            no_agent: false,
        }
    }

    #[test]
    fn the_spec_example_reaches_own_public_windows_only() {
        let v = view(&[&["workspace:own", "class:public"]]);
        assert!(v.visible(&win()));
        let human = WorkspaceFacts {
            id: 1,
            human: true,
            own: false,
        };
        assert!(!v.visible(&WindowFacts {
            workspace: Some(human),
            ..win()
        }));
        assert!(!v.visible(&WindowFacts {
            class: Class::Private,
            ..win()
        }));
    }

    #[test]
    fn no_agent_is_absent_whatever_the_grant_says() {
        let v = view(&[&["workspace:own", "class:secret"], &["handle:7"]]);
        assert!(!v.visible(&WindowFacts {
            no_agent: true,
            ..win()
        }));
    }

    #[test]
    fn unscoped_and_unparseable_lines_reach_nothing() {
        assert!(!view(&[&[]]).visible(&win()));
        assert!(!view(&[&["workspace:own", "colour:blue"]]).visible(&win()));
        assert!(!view(&[&["title:("]]).visible(&win()));
        assert!(!SceneView::default().visible(&win()));
    }

    #[test]
    fn class_is_a_ceiling_defaulting_to_public() {
        let w = WindowFacts {
            class: Class::Private,
            ..win()
        };
        assert!(!view(&[&["handle:7"]]).visible(&w));
        assert!(view(&[&["handle:7", "class:private"]]).visible(&w));
        assert!(view(&[&["handle:7", "class:secret"]]).visible(&w));
        // Two class clauses AND: the tighter one holds.
        assert!(!view(&[&["handle:7", "class:secret", "class:public"]]).visible(&w));
        let s = WindowFacts {
            class: Class::Secret,
            ..win()
        };
        assert!(!view(&[&["handle:7", "class:private"]]).visible(&s));
    }

    #[test]
    fn lines_or_together_and_never_loosen_each_other() {
        let v = view(&[&["app_id:org.gnome.*"], &["workspace:own", "title:Invoices*"]]);
        // Second line: title regex `Invoices*` anchored means "Invoice" + s*.
        assert!(!v.visible(&win()));
        let v = view(&[&["app_id:org.gnome.*"], &["workspace:own", "title:Invoices.*"]]);
        assert!(v.visible(&win()));
        // The app_id line alone cannot reach a firefox window.
        let v = view(&[&["app_id:org.gnome.*"]]);
        assert!(!v.visible(&win()));
    }

    #[test]
    fn title_is_anchored() {
        let w = WindowFacts {
            title: "Inbox — evil.example",
            ..win()
        };
        assert!(!view(&[&["title:Inbox"]]).visible(&w));
        assert!(view(&[&["title:Inbox.*"]]).visible(&w));
    }

    #[test]
    fn content_scopes_match_no_window() {
        assert!(!view(&[&["handle:7", "url:*.example.com/*"]]).visible(&win()));
        assert!(!view(&[&["node_role:button"]]).visible(&win()));
    }

    #[test]
    fn output_and_workspace_selectors() {
        assert!(view(&[&["output:DP-1"]]).visible(&win()));
        assert!(view(&[&["output:1"]]).visible(&win()));
        assert!(view(&[&["output:physical"]]).visible(&win()));
        assert!(!view(&[&["output:virtual"]]).visible(&win()));
        assert!(view(&[&["workspace:3"]]).visible(&win()));
        assert!(!view(&[&["workspace:human"]]).visible(&win()));
        let unplaced = WindowFacts {
            workspace: None,
            output: None,
            ..win()
        };
        assert!(!view(&[&["workspace:own"]]).visible(&unplaced));
        assert!(!view(&[&["output:physical"]]).visible(&unplaced));
    }

    #[test]
    fn principal_scope() {
        let other = WindowFacts {
            launched_by_self: false,
            ..win()
        };
        assert!(!view(&[&["principal:launched_by_self"]]).visible(&other));
        assert!(view(&[&["principal:any"]]).visible(&other));
    }

    #[test]
    fn only_scene_list_lines_count() {
        let mut g = grant(&[]);
        g.capabilities.push(Capability {
            name: "scene.read".into(),
            scopes: vec!["handle:7".into()],
            quota: None,
        });
        assert!(!SceneView::compile([&g]).visible(&win()));
        // And the read view counts only its own capability's lines.
        assert!(SceneView::compile_for(SCENE_READ, [&g]).visible(&win()));
        assert!(!SceneView::compile_for(SCENE_READ, [&grant(&[&["handle:7"]])]).visible(&win()));
    }

    #[test]
    fn glob() {
        let g = Glob::new("org.*.fire?ox");
        assert!(g.matches("org.mozilla.firefox"));
        assert!(!g.matches("org.mozilla.firefoxx"));
        assert!(Glob::new("*").matches(""));
        assert!(Glob::new("a*b*c").matches("aXXbYYc"));
        assert!(!Glob::new("a*b*c").matches("aXXbYY"));
        assert!(Glob::new("[x]").matches("[x]"));
    }

    #[test]
    fn parse_errors() {
        assert_eq!(Scope::parse("workspace").unwrap_err(), ScopeError::Malformed);
        assert_eq!(Scope::parse("workspace:").unwrap_err(), ScopeError::Malformed);
        assert_eq!(
            Scope::parse("argv0:/bin/sh").unwrap_err(),
            ScopeError::UnknownKind
        );
        assert_eq!(Scope::parse("workspace:two").unwrap_err(), ScopeError::BadValue);
        assert_eq!(Scope::parse("class:top").unwrap_err(), ScopeError::BadValue);
        assert_eq!(Scope::parse("principal:me").unwrap_err(), ScopeError::BadValue);
    }
}
