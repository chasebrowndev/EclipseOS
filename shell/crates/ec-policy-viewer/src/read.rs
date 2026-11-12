// SPDX-License-Identifier: AGPL-3.0-only
//! Reading `policy.kdl` off disk.
//!
//! The viewer never asks the compositor for this. `get_config {file:"policy"}`
//! is default-closed in `ipc/gate.rs` and stays closed: the security surface is
//! readable because the file is readable, not because a socket grants it. So
//! the search path from `abyss`'s `config::search_path` is re-derived here,
//! policy entries only, and parsed with the same `kdl` crate.
//!
//! Which keys and windowrule actions are policy-owned is not restated here: it
//! is read from the schema (`ec_abyss_config::schema`, the `Policy` owner),
//! which links no compositor. One the viewer cannot show yet is named in
//! [`NOT_SHOWN_KEYS`] / [`NOT_SHOWN_RULES`]; a policy item in neither that this
//! reader does not pick up fails the tests below.

use std::path::{Path, PathBuf};

use ec_abyss_config::schema::{Owner, RULE_ACTIONS, TABLE};
use kdl::{KdlDocument, KdlNode, KdlValue};

/// Every policy-owned scalar key, from the schema.
pub fn policy_keys() -> impl Iterator<Item = &'static str> {
    TABLE.iter().filter(|k| k.owner == Owner::Policy).map(|k| k.path)
}

/// Every policy-owned windowrule action, from the schema.
pub fn policy_rules() -> impl Iterator<Item = &'static str> {
    RULE_ACTIONS
        .iter()
        .filter(|(_, o)| *o == Owner::Policy)
        .map(|(a, _)| *a)
}

/// Policy keys this viewer does not read or show yet. Each needs a reader
/// here and a section in `view.rs`; until then it is listed, so a new policy
/// key cannot go missing without a line saying so.
pub const NOT_SHOWN_KEYS: &[&str] = &[
    // ADR 0056 layer hiding: no section in the viewer yet.
    "capture.hide-layer",
];

/// Policy windowrule actions this viewer does not show yet, as above.
pub const NOT_SHOWN_RULES: &[&str] = &[
    // S-06 §3.3 app-capable fallback opt-out: not listed in the viewer yet.
    "irreversible-capable",
];

/// Whether a windowrule with this verb is shown: policy-owned and not
/// [`NOT_SHOWN_RULES`].
fn shown_rule(verb: &str) -> bool {
    !NOT_SHOWN_RULES.contains(&verb) && policy_rules().any(|r| r == verb)
}

/// Where one line came from: which file, and which line of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub file: PathBuf,
    pub line: usize,
}

impl Source {
    /// `policy.kdl:41` — the file's own name, not its whole path, because the
    /// path is already the section heading.
    pub fn short(&self) -> String {
        let name = self
            .file
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.file.display().to_string());
        format!("{name}:{}", self.line)
    }
}

/// One allowed name, and where it was allowed.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub source: Source,
}

/// A list-valued policy key.
///
/// The distinction between "no node in any file" and "a node listing nothing"
/// matters only for the sentence shown to the reader; both deny everyone.
#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    pub declared: Option<Source>,
    pub entries: Vec<Entry>,
}

impl Allowlist {
    /// The sentence for an allowlist with nothing in it.
    ///
    /// An empty allowlist is not an absence of policy — it is the strictest
    /// policy there is, and must read that way. Never a blank list, never
    /// "none configured".
    pub fn empty_reads_as(&self) -> &'static str {
        match self.declared {
            Some(_) => "Listed empty — every client is denied.",
            None => "Not set — every client is denied.",
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One `windowrule` whose action the policy file owns.
#[derive(Debug, Clone)]
pub struct Rule {
    /// `app-trust`, `sensitivity`, …
    pub verb: String,
    /// The parameter after the verb, if the action takes one.
    pub param: Option<String>,
    /// `app-id` / `title` matchers, in file order.
    pub matchers: Vec<(String, String)>,
    pub source: Source,
}

impl Rule {
    pub fn action(&self) -> String {
        match &self.param {
            Some(p) => format!("{} {p}", self.verb),
            None => self.verb.clone(),
        }
    }
}

/// What one file on the search path turned out to be.
#[derive(Debug, Clone)]
pub enum FileState {
    Missing,
    Unreadable(String),
    Malformed(String),
    Read,
}

#[derive(Debug, Clone)]
pub struct FileReport {
    pub path: PathBuf,
    pub state: FileState,
}

/// Everything the viewer shows, in the order the files were read.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub files: Vec<FileReport>,
    pub scripted_input: Option<(bool, Source)>,
    pub data_control_allow: Allowlist,
    pub capture_allow: Allowlist,
    pub capture_redact: Allowlist,
    pub capture_hide_layer: Allowlist,
    pub input_method_allow: Allowlist,
    pub virtual_pointer_allow: Allowlist,
    pub rules: Vec<Rule>,
}

/// `/etc/eclipse/policy.kdl`, then the user's own — later files win, which is
/// why they are parsed in this order and each key is simply overwritten.
pub fn search_path() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/etc/eclipse/policy.kdl")];
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(dir) = user {
        v.push(dir.join("eclipse").join("policy.kdl"));
    }
    v
}

/// Read and merge every file on the search path.
pub fn load() -> Policy {
    let mut policy = Policy::default();
    for path in search_path() {
        let state = match std::fs::read_to_string(&path) {
            Ok(text) => match text.parse::<KdlDocument>() {
                Ok(doc) => {
                    merge(&mut policy, &doc, &text, &path);
                    FileState::Read
                }
                Err(e) => FileState::Malformed(first_line(&e.to_string())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileState::Missing,
            Err(e) => FileState::Unreadable(e.to_string()),
        };
        policy.files.push(FileReport { path, state });
    }
    policy
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("unparseable").trim().to_owned()
}

/// Line number for a byte offset. One pass over the prefix — these files are
/// tens of lines, and this runs once per entry at load.
fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset.min(text.len()))
        .map_or(1, |p| p.bytes().filter(|b| *b == b'\n').count() + 1)
}

fn source(text: &str, node: &KdlNode, path: &Path) -> Source {
    Source {
        file: path.to_path_buf(),
        line: line_of(text, node.span().offset()),
    }
}

fn strings(node: &KdlNode) -> Vec<String> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .filter_map(|e| e.value().as_string().map(str::to_owned))
        .collect()
}

fn first_arg(node: &KdlNode) -> Option<&KdlValue> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .map(|e| e.value())
}

fn allowlist(node: &KdlNode, text: &str, path: &Path) -> Allowlist {
    let src = source(text, node, path);
    Allowlist {
        entries: strings(node)
            .into_iter()
            .map(|name| Entry {
                name,
                source: src.clone(),
            })
            .collect(),
        declared: Some(src),
    }
}

/// Fold one parsed document into `policy`. Unknown nodes are ignored rather
/// than reported: this crate reads the security surface, and `abyss` is the one
/// that refuses a bad file.
fn merge(policy: &mut Policy, doc: &KdlDocument, text: &str, path: &Path) {
    for node in doc.nodes() {
        match node.name().value() {
            "misc" => {
                for n in node.children().iter().flat_map(|c| c.nodes()) {
                    if n.name().value() == "scripted-input" {
                        let on = first_arg(n).and_then(KdlValue::as_bool).unwrap_or(false);
                        policy.scripted_input = Some((on, source(text, n, path)));
                    }
                }
            }
            "clipboard" => {
                for n in node.children().iter().flat_map(|c| c.nodes()) {
                    if n.name().value() == "data-control-allow" {
                        policy.data_control_allow = allowlist(n, text, path);
                    }
                }
            }
            "capture" => {
                for n in node.children().iter().flat_map(|c| c.nodes()) {
                    match n.name().value() {
                        "allow" => policy.capture_allow = allowlist(n, text, path),
                        "redact-app-id" => policy.capture_redact = allowlist(n, text, path),
                        "hide-layer" => policy.capture_hide_layer = allowlist(n, text, path),
                        _ => {}
                    }
                }
            }
            "input-method" => {
                for n in node.children().iter().flat_map(|c| c.nodes()) {
                    if n.name().value() == "allow" {
                        policy.input_method_allow = allowlist(n, text, path);
                    }
                }
            }
            "virtual-pointer" => {
                for n in node.children().iter().flat_map(|c| c.nodes()) {
                    if n.name().value() == "allow" {
                        policy.virtual_pointer_allow = allowlist(n, text, path);
                    }
                }
            }
            "windowrule" => {
                if let Some(rule) = windowrule(node, text, path) {
                    policy.rules.push(rule);
                }
            }
            _ => {}
        }
    }
}

/// `windowrule "app-trust trusted" { app-id "org.x.Vault" }` — the action is
/// one space-separated string, the matchers are child nodes. Rules whose verb
/// belongs to `abyss.kdl` are skipped: this window shows the security surface
/// and nothing else.
fn windowrule(node: &KdlNode, text: &str, path: &Path) -> Option<Rule> {
    let action = first_arg(node)?.as_string()?;
    let mut parts = action.split_whitespace();
    let verb = parts.next()?.to_owned();
    if !shown_rule(&verb) {
        return None;
    }
    let param = parts.next().map(str::to_owned);

    let matchers = node
        .children()
        .iter()
        .flat_map(|c| c.nodes())
        .filter(|n| matches!(n.name().value(), "app-id" | "title"))
        .filter_map(|n| {
            let pat = first_arg(n)?.as_string()?.to_owned();
            Some((n.name().value().to_owned(), pat))
        })
        .collect();

    Some(Rule {
        verb,
        param,
        matchers,
        source: source(text, node, path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Policy {
        let mut p = Policy::default();
        let doc: KdlDocument = text.parse().expect("test fixture parses");
        merge(&mut p, &doc, text, Path::new("/etc/eclipse/policy.kdl"));
        p
    }

    #[test]
    fn an_empty_allowlist_says_it_denies_everyone() {
        let listed = parse("capture {\n  allow\n}\n");
        assert!(listed.capture_allow.is_empty());
        assert_eq!(
            listed.capture_allow.empty_reads_as(),
            "Listed empty — every client is denied."
        );

        let absent = parse("capture {\n  redact-app-id \"org.x.Vault\"\n}\n");
        assert!(absent.capture_allow.is_empty());
        assert_eq!(
            absent.capture_allow.empty_reads_as(),
            "Not set — every client is denied."
        );
    }

    #[test]
    fn an_allowed_name_carries_the_line_it_came_from() {
        let p = parse("capture {\n  allow \"obs\" \"grim\"\n}\n");
        let names: Vec<&str> = p.capture_allow.entries.iter().map(|e| &*e.name).collect();
        assert_eq!(names, ["obs", "grim"]);
        assert_eq!(p.capture_allow.entries[0].source.line, 2);
        assert_eq!(p.capture_allow.entries[0].source.short(), "policy.kdl:2");
    }

    #[test]
    fn a_compositor_owned_windowrule_is_not_a_security_rule() {
        let p = parse(
            "windowrule \"opacity 0.9\" {\n  app-id \"kitty\"\n}\nwindowrule \"app-trust trusted\" {\n  app-id \"org.x.Vault\"\n}\n",
        );
        assert_eq!(p.rules.len(), 1);
        assert_eq!(p.rules[0].action(), "app-trust trusted");
        assert_eq!(p.rules[0].matchers, [("app-id".into(), "org.x.Vault".into())]);
        assert_eq!(p.rules[0].source.line, 4);
    }

    #[test]
    fn a_rule_without_a_parameter_still_reads() {
        let p = parse("windowrule \"no-agent\" {\n  title \"Bank.*\"\n}\n");
        assert_eq!(p.rules[0].action(), "no-agent");
    }

    #[test]
    fn scripted_input_defaults_to_off_when_the_file_is_silent() {
        assert!(parse("capture {\n  allow\n}\n").scripted_input.is_none());
        assert!(
            parse("misc {\n  scripted-input #true\n}\n")
                .scripted_input
                .unwrap()
                .0
        );
    }

    #[test]
    fn hidden_layers_read_beside_the_capture_allowlist() {
        let p = parse("capture {\n  allow \"grim\"\n  hide-layer \"ec-hyperion-bar:eclipse-eye\"\n}\n");
        let names: Vec<&str> = p.capture_hide_layer.entries.iter().map(|e| &*e.name).collect();
        assert_eq!(names, ["ec-hyperion-bar:eclipse-eye"]);
        assert_eq!(p.capture_hide_layer.entries[0].source.line, 3);
        assert!(parse("capture {\n  allow\n}\n").capture_hide_layer.is_empty());
    }

    #[test]
    fn the_injected_input_allowlists_read() {
        let p = parse("input-method {\n  allow \"fcitx5\" \"kime\"\n}\nvirtual-pointer {\n  allow\n}\n");
        let names: Vec<&str> = p.input_method_allow.entries.iter().map(|e| &*e.name).collect();
        assert_eq!(names, ["fcitx5", "kime"]);
        assert_eq!(p.input_method_allow.entries[0].source.line, 2);
        assert!(p.virtual_pointer_allow.is_empty());
        assert!(p.virtual_pointer_allow.declared.is_some());
    }

    #[test]
    fn garbage_is_a_report_not_a_panic() {
        assert!("windowrule {{{".parse::<KdlDocument>().is_err());
    }

    /// A `policy.kdl` (or `abyss.kdl`) fragment setting `path` to a value of
    /// its schema type, nested as the dotted path is.
    fn fragment(path: &str) -> String {
        use ec_abyss_config::schema::{get_key, Ty};
        let value = match get_key(path).expect("a schema key").ty {
            Ty::Bool => "#true",
            Ty::Int { .. } => "1",
            Ty::Float { .. } => "0.5",
            _ => "\"probe\"",
        };
        let parts: Vec<&str> = path.split('.').collect();
        let (leaf, nodes) = parts.split_last().expect("a path has a leaf");
        let mut out = String::new();
        for n in nodes {
            out.push_str(n);
            out.push_str(" {\n");
        }
        out.push_str(&format!("{leaf} {value}\n"));
        for _ in nodes {
            out.push_str("}\n");
        }
        out
    }

    /// Whether `path` reached the [`Policy`] the view draws. A policy key
    /// the schema grows lands in no arm and so fails the test below until it
    /// is read and shown, or listed in [`NOT_SHOWN_KEYS`].
    fn landed(p: &Policy, path: &str) -> bool {
        match path {
            "misc.scripted-input" => p.scripted_input.is_some(),
            "clipboard.data-control-allow" => p.data_control_allow.declared.is_some(),
            "capture.allow" => p.capture_allow.declared.is_some(),
            "capture.redact-app-id" => p.capture_redact.declared.is_some(),
            "input-method.allow" => p.input_method_allow.declared.is_some(),
            "virtual-pointer.allow" => p.virtual_pointer_allow.declared.is_some(),
            _ => false,
        }
    }

    fn untouched(p: &Policy) -> bool {
        p.scripted_input.is_none()
            && p.data_control_allow.declared.is_none()
            && p.capture_allow.declared.is_none()
            && p.capture_redact.declared.is_none()
            && p.input_method_allow.declared.is_none()
            && p.virtual_pointer_allow.declared.is_none()
            && p.rules.is_empty()
    }

    #[test]
    fn every_schema_policy_key_is_shown_or_listed_as_not_shown() {
        assert!(policy_keys().count() > 0);
        for path in policy_keys() {
            let p = parse(&fragment(path));
            if NOT_SHOWN_KEYS.contains(&path) {
                assert!(
                    !landed(&p, path),
                    "{path} is shown now: drop it from NOT_SHOWN_KEYS"
                );
            } else {
                assert!(
                    landed(&p, path),
                    "{path} is policy-owned and the viewer does not show it"
                );
            }
        }
        for path in NOT_SHOWN_KEYS {
            assert!(policy_keys().any(|k| k == *path), "{path} is not a policy key");
        }
    }

    #[test]
    fn no_abyss_owned_key_is_read_as_policy() {
        for key in TABLE.iter().filter(|k| k.owner == Owner::Abyss) {
            let p = parse(&fragment(key.path));
            assert!(
                untouched(&p),
                "{} is abyss-owned but the viewer read it",
                key.path
            );
        }
    }

    #[test]
    fn the_rules_shown_are_the_schema_policy_rules() {
        for (verb, owner) in RULE_ACTIONS {
            let p = parse(&format!("windowrule \"{verb} x\" {{\n  app-id \"a\"\n}}\n"));
            let want = *owner == Owner::Policy && !NOT_SHOWN_RULES.contains(verb);
            assert_eq!(p.rules.len() == 1, want, "{verb}");
        }
        for verb in NOT_SHOWN_RULES {
            assert!(policy_rules().any(|r| r == *verb), "{verb} is not a policy rule");
        }
    }

    #[test]
    fn the_search_path_is_policy_files_only() {
        for p in search_path() {
            assert_eq!(p.file_name().unwrap(), "policy.kdl");
        }
    }
}
