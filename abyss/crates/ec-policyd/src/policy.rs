// SPDX-License-Identifier: AGPL-3.0-only
//! Policy source → enforcement table (S-02 §3–§4). TCB.
//!
//! Reads every `*.kdl` in `/etc/eclipse/policy/` (system) and then
//! `$XDG_CONFIG_HOME/eclipse/policy/` (user), in name order, and compiles the
//! `rule` nodes into an [`ec_policy_eval::check::Table`].
//!
//! This build compiles the **fast-path subset**: exactly the predicates
//! `ec_policy_eval::check` can answer in the compositor. Anything else is a
//! compile error, never a skipped line, and a failed compile keeps the
//! previous table live (S-02 §4). The rest of S-02 is refused by name until
//! it is built:
//!
//! - `defer`-only and unbuilt predicates (`time`, `rate_gt`, `grant_scope`,
//!   `target_handle`, `provenance_min_trust`/`_head`/`_age_gt`/`_mismatch`,
//!   `egress_bytes_gt`, `secret_bound_host_mismatch`, `node_credential`).
//! - `classify` and `trust` (S-05 §3.1, §6) compile into the table's
//!   classifier, with matchers `app_id`, `title` (regex), `url`, `output`,
//!   `launched_by` and `xwayland`. Matchers that need the semantic tree
//!   (`node_role`, `node_name`) or facts abyss does not hold yet
//!   (`workspace`, `pid_exe_hash`, `path`) are compile errors: dropping a
//!   raising line would leave content under-classified, which fails open.
//! - `defaults { sensitivity }` sets the default class (`private` or
//!   `secret`, never `public`). Its other keys, and `irreversible`,
//!   `profile`, `dedupe_exclude` and `trusted_endpoint`, are reported as
//!   **not yet enforced**.
//!
//! Syntax, where S-02's sketch is not KDL:
//!
//! - A regex predicate (`title`, `node_name`) takes the regex as its
//!   argument: `node_name "(?i)^(send|post)$"`.
//! - `provenance_contains trust="untrusted"` or `source="channel"`.
//! - `unless grant_has "capture.secret" unattended=#true` (a child node).
//!
//! Source order is kept within a phase (first match wins). User rules follow
//! system rules in each phase; since phases run deny first, a user file can
//! only tighten a system one unless it adds an `allow`, and an `allow` cannot
//! override a system `deny`, `prompt` or `defer` (S-02 §2–§3).

use std::path::PathBuf;

use ec_policy_eval::check::{CompiledRule, Phases, Pred, Table, Trust, Unless};
use ec_policy_eval::classify::{Classifier, ClassifyRule, Matcher, TrustRule};
use ec_policy_eval::scope::Class;
use ec_policy_eval::scope::Glob;
use ec_policy_eval::table::{class_of, regex, trust_of};
use kdl::{KdlDocument, KdlNode, KdlValue};

/// One compile error, with where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub file: String,
    pub rule: Option<String>,
    pub what: String,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rule {
            Some(r) => write!(f, "{}: rule {r:?}: {}", self.file, self.what),
            None => write!(f, "{}: {}", self.file, self.what),
        }
    }
}

/// A compiled table, and what parsed but is not enforced yet.
#[derive(Debug)]
pub struct Compiled {
    pub rules: Phases,
    pub classifier: Classifier,
    pub not_enforced: Vec<String>,
}

/// Policy directories, system first.
pub fn sources() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/etc/eclipse/policy")];
    if let Some(dir) = std::env::var_os("ECLIPSE_POLICY_DIR") {
        // Tests and a nested dev instance point at their own directory only.
        return vec![PathBuf::from(dir)];
    }
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(u) = user {
        dirs.push(u.join("eclipse").join("policy"));
    }
    dirs
}

/// Every `*.kdl` in `dirs`, in directory then name order, with its text. A
/// missing directory contributes nothing; an unreadable file is an error.
pub fn read(dirs: &[PathBuf]) -> Result<Vec<(String, String)>, CompileError> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
            .collect();
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).map_err(|e| CompileError {
                file: f.display().to_string(),
                rule: None,
                what: format!("cannot read: {e}"),
            })?;
            out.push((f.display().to_string(), text));
        }
    }
    Ok(out)
}

/// The newest modification time across `dirs` and their `*.kdl` files, so a
/// watcher can tell that a recompile is due.
pub fn stamp(dirs: &[PathBuf]) -> Option<std::time::SystemTime> {
    let mut newest = None;
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                newest = newest.max(Some(m));
            }
        }
        if let Ok(m) = std::fs::metadata(dir).and_then(|m| m.modified()) {
            newest = newest.max(Some(m));
        }
    }
    newest
}

/// Compile `files` (name, text), in order.
pub fn compile(files: &[(String, String)]) -> Result<Compiled, CompileError> {
    let mut c = Compiled {
        rules: Phases::default(),
        classifier: Classifier::default(),
        not_enforced: Vec::new(),
    };
    for (file, text) in files {
        let doc: KdlDocument = text.parse().map_err(|e: kdl::KdlError| CompileError {
            file: file.clone(),
            rule: None,
            what: format!("not KDL: {e}"),
        })?;
        for node in doc.nodes() {
            let err = |rule: Option<&str>, what: String| CompileError {
                file: file.clone(),
                rule: rule.map(str::to_owned),
                what,
            };
            match node.name().value() {
                "rule" => {
                    let (phase, rule) = compile_rule(node).map_err(|(r, w)| err(r.as_deref(), w))?;
                    let ids = [&c.rules.deny, &c.rules.prompt, &c.rules.defer, &c.rules.allow];
                    if ids.iter().any(|p| p.iter().any(|r| r.id == rule.id)) {
                        return Err(err(Some(&rule.id), "duplicate rule id".into()));
                    }
                    match phase {
                        Phase::Deny => c.rules.deny.push(rule),
                        Phase::Prompt => c.rules.prompt.push(rule),
                        Phase::Defer => c.rules.defer.push(rule),
                        Phase::Allow => c.rules.allow.push(rule),
                    }
                }
                "classify" => {
                    let rule = compile_classify(node).map_err(|w| err(None, w))?;
                    c.classifier.classify.push(rule);
                }
                "trust" => {
                    let rule = compile_trust(node).map_err(|w| err(None, w))?;
                    c.classifier.trust.push(rule);
                }
                "defaults" => {
                    for d in node.children().map(|c| c.nodes()).unwrap_or_default() {
                        if d.name().value() == "sensitivity" {
                            c.classifier.default = match strings(d).as_slice() {
                                ["private"] => Class::Private,
                                ["secret"] => Class::Secret,
                                _ => {
                                    return Err(err(
                                        None,
                                        "defaults sensitivity must be \"private\" or \"secret\"; only a classify \"public\" rule makes something public".into(),
                                    ))
                                }
                            };
                        } else {
                            c.not_enforced.push(format!(
                                "{file}: defaults `{}` parsed, not yet enforced",
                                d.name().value()
                            ));
                        }
                    }
                }
                n @ ("irreversible" | "profile" | "dedupe_exclude" | "trusted_endpoint") => c
                    .not_enforced
                    .push(format!("{file}: `{n}` parsed, not yet enforced")),
                other => return Err(err(None, format!("unknown node `{other}`"))),
            }
        }
    }
    Ok(c)
}

fn compile_matchers(node: &KdlNode, what: &str) -> Result<Vec<Matcher>, String> {
    let Some(children) = node.children() else {
        return Err(format!("{what} needs a matcher block"));
    };
    let mut out = Vec::new();
    for m in children.nodes() {
        let name = m.name().value();
        let vals = strings(m);
        let globs = || -> Result<Vec<Glob>, String> {
            if vals.is_empty() || has_props(m) {
                return Err(format!("{what}: `{name}` takes one or more string arguments"));
            }
            Ok(vals.iter().map(|v| Glob::new(v)).collect())
        };
        out.push(match name {
            "app_id" => Matcher::AppId(globs()?),
            "url" => Matcher::Url(globs()?),
            "output" => Matcher::Output(globs()?),
            "launched_by" => Matcher::LaunchedBy(globs()?),
            "title" => match vals.as_slice() {
                [src] if !has_props(m) => {
                    Matcher::Title(regex(src).map_err(|_| format!("{what}: the title regex does not compile"))?)
                }
                _ => return Err(format!("{what}: `title` takes one regex argument")),
            },
            "xwayland" => Matcher::Xwayland(
                m.entries()
                    .first()
                    .filter(|e| e.name().is_none() && m.entries().len() == 1)
                    .and_then(|e| e.value().as_bool())
                    .ok_or(format!("{what}: `xwayland` takes #true or #false"))?,
            ),
            n @ ("node_role" | "node_name") => {
                return Err(format!(
                    "{what}: `{n}` needs the semantic tree (COMP-09), which is not built; the rule is refused rather \
                     than compiled without its raise"
                ))
            }
            n @ ("workspace" | "pid_exe_hash" | "path") => {
                return Err(format!("{what}: `{n}` is not supported by this build's classifier"))
            }
            other => return Err(format!("{what}: unknown matcher `{other}`")),
        });
    }
    if out.is_empty() {
        return Err(format!("{what} needs at least one matcher"));
    }
    Ok(out)
}

fn compile_classify(node: &KdlNode) -> Result<ClassifyRule, String> {
    let class = match strings(node).as_slice() {
        [c] => class_of(c).ok_or(format!("classify: unknown class `{c}`"))?,
        _ => return Err("classify takes one class: classify \"secret\" { ... }".into()),
    };
    let what = format!("classify {class:?}");
    Ok(ClassifyRule {
        class,
        matchers: compile_matchers(node, &what)?,
    })
}

fn compile_trust(node: &KdlNode) -> Result<TrustRule, String> {
    let trust = match strings(node).as_slice() {
        [t] => match trust_of(t) {
            Some(Trust::Human) | None => {
                return Err(format!("trust: `{t}` is not untrusted, standard or trusted"))
            }
            Some(t) => t,
        },
        _ => return Err("trust takes one level: trust \"trusted\" { ... }".into()),
    };
    let what = format!("trust {trust:?}");
    Ok(TrustRule {
        trust,
        matchers: compile_matchers(node, &what)?,
    })
}

enum Phase {
    Deny,
    Prompt,
    Defer,
    Allow,
}

type RuleError = (Option<String>, String);

fn strings(node: &KdlNode) -> Vec<&str> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .filter_map(|e| e.value().as_string())
        .collect()
}

fn has_props(node: &KdlNode) -> bool {
    node.entries().iter().any(|e| e.name().is_some())
}

fn prop<'a>(node: &'a KdlNode, name: &str) -> Option<&'a KdlValue> {
    node.entries()
        .iter()
        .find(|e| e.name().is_some_and(|n| n.value() == name))
        .map(|e| e.value())
}

fn compile_rule(node: &KdlNode) -> Result<(Phase, CompiledRule), RuleError> {
    let args: Vec<&KdlValue> = node
        .entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect();
    let id = args
        .first()
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty())
        .ok_or((
            None,
            "a rule needs an id: rule \"<id>\" <phase> { ... }".to_owned(),
        ))?
        .to_owned();
    let e = |what: &str| (Some(id.clone()), what.to_owned());
    if args.len() != 2 || has_props(node) {
        return Err(e("a rule takes exactly an id and a phase"));
    }
    let phase = match args[1].as_string() {
        Some("deny") => Phase::Deny,
        Some("prompt") => Phase::Prompt,
        Some("defer") => Phase::Defer,
        Some("allow") => Phase::Allow,
        _ => return Err(e("phase must be deny, prompt, defer or allow")),
    };
    let Some(children) = node.children() else {
        return Err(e("a rule needs a predicate block"));
    };
    let mut preds = Vec::new();
    let mut unless = Vec::new();
    for p in children.nodes() {
        let name = p.name().value();
        if name == "unless" {
            unless.extend(compile_unless(p).map_err(|w| e(&w))?);
            continue;
        }
        preds.push(compile_pred(p).map_err(|w| e(&w))?);
    }
    if preds.is_empty() {
        // A rule with no predicate matches everything: legal, but only as an
        // explicit `capability "*"`, so it is never an accident.
        return Err(e(
            "a rule needs at least one predicate; write capability \"*\" to match everything",
        ));
    }
    Ok((phase, CompiledRule { id, preds, unless }))
}

fn compile_unless(node: &KdlNode) -> Result<Vec<Unless>, String> {
    let children = node
        .children()
        .ok_or("unless takes a block: unless { grant_has \"<cap>\" }")?;
    let mut out = Vec::new();
    for g in children.nodes() {
        if g.name().value() != "grant_has" {
            return Err(format!(
                "unless may only test grant facts (grant_has), not `{}`",
                g.name().value()
            ));
        }
        let caps = strings(g);
        let unattended = match prop(g, "unattended") {
            None => false,
            Some(v) => v.as_bool().ok_or("unattended must be #true or #false")?,
        };
        if caps.len() != 1 || g.entries().len() > 2 {
            return Err("grant_has takes one capability and an optional unattended=".into());
        }
        out.push(Unless {
            capability: caps[0].to_owned(),
            unattended,
        });
    }
    Ok(out)
}

const DEFER_ONLY: [&str; 12] = [
    "time",
    "rate_gt",
    "grant_scope",
    "target_handle",
    "provenance_min_trust",
    "provenance_head",
    "provenance_age_gt",
    "provenance_mismatch",
    "egress_bytes_gt",
    "secret_bound_host_mismatch",
    "node_credential",
    "terminal_cmd",
];

fn compile_pred(node: &KdlNode) -> Result<Pred, String> {
    let name = node.name().value();
    let vals = strings(node);
    let globs = || -> Result<Vec<Glob>, String> {
        if vals.is_empty() || has_props(node) {
            return Err(format!("`{name}` takes one or more string arguments"));
        }
        Ok(vals.iter().map(|v| Glob::new(v)).collect())
    };
    let words = || -> Result<Vec<String>, String> {
        if vals.is_empty() || has_props(node) {
            return Err(format!("`{name}` takes one or more string arguments"));
        }
        Ok(vals.iter().map(|v| (*v).to_owned()).collect())
    };
    let one_re = || -> Result<regex::Regex, String> {
        match vals.as_slice() {
            [src] if !has_props(node) => {
                regex(src).map_err(|_| format!("`{name}`: the regex does not compile"))
            }
            _ => Err(format!("`{name}` takes one regex argument")),
        }
    };
    Ok(match name {
        "capability" => Pred::Capability(globs()?),
        "target_class" => Pred::TargetClass(
            words()?
                .iter()
                .map(|c| class_of(c).ok_or(format!("unknown class `{c}`")))
                .collect::<Result<_, _>>()?,
        ),
        "target_app_id" => Pred::TargetAppId(globs()?),
        "title" => Pred::Title(one_re()?),
        "node_role" => Pred::NodeRole(words()?),
        "node_name" => Pred::NodeName(one_re()?),
        "node_source" => Pred::NodeSource(words()?),
        "node_confidence_lt" => {
            let v = node
                .entries()
                .first()
                .filter(|e| e.name().is_none() && node.entries().len() == 1)
                .and_then(|e| {
                    e.value()
                        .as_float()
                        .or_else(|| e.value().as_integer().map(|i| i as f64))
                })
                .filter(|v| (0.0..=1.0).contains(v))
                .ok_or("node_confidence_lt takes one number from 0 to 1")?;
            Pred::NodeConfidenceLt(v as f32)
        }
        "irreversible" => Pred::Irreversible(globs()?),
        "url" => Pred::Url(globs()?),
        "principal" => Pred::Principal(globs()?),
        "principal_profile" => Pred::PrincipalProfile(words()?),
        "app_trust" => Pred::AppTrust(
            words()?
                .iter()
                .map(|t| trust_of(t).ok_or(format!("unknown trust `{t}`")))
                .collect::<Result<_, _>>()?,
        ),
        "app_irreversible_capable" => {
            let b = node
                .entries()
                .first()
                .filter(|e| e.name().is_none() && node.entries().len() == 1)
                .and_then(|e| e.value().as_bool())
                .ok_or("app_irreversible_capable takes #true or #false")?;
            Pred::AppIrreversibleCapable(b)
        }
        "provenance_contains" => {
            if node.entries().len() != 1 {
                return Err("provenance_contains takes one of trust=\"…\" or source=\"…\"".into());
            }
            match (prop(node, "trust"), prop(node, "source")) {
                (Some(t), None) => Pred::ProvenanceContainsTrust(
                    t.as_string()
                        .and_then(trust_of)
                        .ok_or("provenance_contains trust= must be a trust level")?,
                ),
                (None, Some(s)) => Pred::ProvenanceContainsSource(
                    s.as_string()
                        .ok_or("provenance_contains source= must be a string")?
                        .to_owned(),
                ),
                _ => return Err("provenance_contains takes one of trust=\"…\" or source=\"…\"".into()),
            }
        }
        "provenance_absent" => {
            if !node.entries().is_empty() {
                return Err("provenance_absent takes no arguments".into());
            }
            Pred::ProvenanceAbsent
        }
        n if DEFER_ONLY.contains(&n) => {
            return Err(format!(
                "`{n}` is not supported in this build's enforcement table; the rule is refused rather than \
                 compiled without it"
            ))
        }
        other => return Err(format!("unknown predicate `{other}`")),
    })
}

/// Compile and stamp a table at `version`.
pub fn table(compiled: Compiled, version: u64) -> Table {
    Table {
        version,
        rules: compiled.rules,
        classifier: compiled.classifier,
    }
}

/// The default policy shipped with policyd, for an installation with no
/// policy files: the S-02 §3 example's fast-path rules.
pub const DEFAULT_POLICY: &str = include_str!("default-policy.kdl");

/// Whether `dir` holds no policy file, so the default applies.
pub fn is_empty(dirs: &[PathBuf]) -> bool {
    read(dirs).map(|f| f.is_empty()).unwrap_or(false)
}

/// The files to compile: the sources, or the shipped default when there are
/// none.
pub fn load(dirs: &[PathBuf]) -> Result<Vec<(String, String)>, CompileError> {
    let files = read(dirs)?;
    if files.is_empty() {
        return Ok(vec![("<default policy>".into(), DEFAULT_POLICY.to_owned())]);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_policy_eval::check::{check, Outcome, ProvenanceFacts, RequestCtx, Trust};
    use ec_policy_eval::scope::Class;

    fn one(text: &str) -> Result<Compiled, CompileError> {
        compile(&[("t.kdl".into(), text.into())])
    }

    #[test]
    fn the_default_policy_compiles() {
        let c = one(DEFAULT_POLICY).expect("default policy");
        assert!(!c.rules.deny.is_empty() && !c.rules.prompt.is_empty() && !c.rules.allow.is_empty());
    }

    #[test]
    fn a_compiled_rule_decides() {
        let c = one(r#"
            rule "no-secret-capture" deny {
                capability "capture.*"
                target_class "secret"
                unless { grant_has "capture.secret" unattended=#true }
            }
            rule "vision" prompt {
                capability "click"
                node_source "vision"
                node_name "(?i)^send$"
                node_confidence_lt 0.9
            }
            rule "laundering" defer {
                capability "seat.action"
                provenance_contains source="channel"
            }
            rule "default-allow" allow { capability "*"; }
            "#)
        .unwrap();
        let t = table(c, 1);
        let mut ctx = RequestCtx {
            principal: "agent:a",
            profile: "operator",
            grants: &[],
            capability: "capture.window",
            app_id: None,
            title: None,
            class: Class::Secret,
            app_trust: Trust::Standard,
            app_irreversible_capable: false,
            node: None,
            url: None,
            irreversible: None,
            provenance: ProvenanceFacts {
                empty: false,
                trusts: [false; 4],
                sources: &[],
            },
        };
        assert_eq!(check(&t, &ctx).rule, "no-secret-capture");
        ctx.class = Class::Private;
        assert_eq!(check(&t, &ctx).outcome, Outcome::Allow);
        let channel = ["channel"];
        ctx.capability = "seat.action";
        ctx.provenance.sources = &channel;
        assert_eq!(check(&t, &ctx).outcome, Outcome::Defer);
    }

    #[test]
    fn unsupported_and_unknown_things_are_errors_not_skips() {
        for bad in [
            r#"rule "r" deny { time "09:00-17:00"; }"#,
            r#"rule "r" deny { rate_gt 3; }"#,
            r#"rule "r" deny { wobble "x"; }"#,
            r#"rule "r" maybe { capability "*"; }"#,
            r#"rule "r" deny { }"#,
            r#"rule "r" deny { capability "a"; unless { target_class "secret" } }"#,
            r#"rule "r" deny { node_name "(unclosed"; }"#,
            r#"rule "r" deny { target_class "topsecret"; }"#,
            r#"rule "r" deny { capability "a"; }
               rule "r" allow { capability "b"; }"#,
            r#"frobnicate "x""#,
            "rule \"r\" deny {",
        ] {
            assert!(one(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn classify_trust_and_defaults_compile_and_unbuilt_matchers_are_refused() {
        let c = one(r#"defaults { sensitivity "private"; prompt_timeout "120s"; }
               classify "secret" { app_id "org.keepassxc.KeePassXC"; title "(?i)password"; }
               classify "public" { url "https://*.wikipedia.org/*"; }
               trust "trusted" { app_id "foot"; }
               trust "untrusted" { app_id "steam"; url "http://*"; }
               irreversible "communication.send" { url "mail.google.com"; }
               rule "a" allow { capability "*"; }"#)
        .unwrap();
        assert_eq!(c.classifier.classify.len(), 2);
        assert_eq!(c.classifier.trust.len(), 2);
        assert_eq!(c.classifier.default, Class::Private);
        assert_eq!(c.not_enforced.len(), 2, "prompt_timeout and irreversible");
        for bad in [
            r#"classify "secret" { node_role "password"; }"#,
            r#"classify "secret" { workspace 3; }"#,
            r#"classify "secret" { }"#,
            r#"classify "topsecret" { app_id "x"; }"#,
            r#"trust "human" { app_id "x"; }"#,
            r#"defaults { sensitivity "public"; }"#,
        ] {
            assert!(one(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_policy_directory_is_read_in_name_order_and_empty_means_default() {
        let dir = std::env::temp_dir().join(format!("ec-policy-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dirs = vec![dir.clone()];
        assert!(is_empty(&dirs));
        assert_eq!(load(&dirs).unwrap()[0].0, "<default policy>");
        std::fs::write(dir.join("b.kdl"), r#"rule "b" allow { capability "*"; }"#).unwrap();
        std::fs::write(dir.join("a.kdl"), r#"rule "a" deny { capability "x"; }"#).unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let files = read(&dirs).unwrap();
        assert!(files[0].0.ends_with("a.kdl") && files[1].0.ends_with("b.kdl"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole path a table takes: compiled here, signed, framed as a link
    /// message, then decoded, verified and evaluated as abyss does.
    #[test]
    fn the_default_policy_survives_the_wire_and_decides_the_same() {
        use ec_policy_eval::link::FromPolicyd;
        use ec_policy_eval::table as wire;
        let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let t = table(one(DEFAULT_POLICY).unwrap(), 42);
        let bytes = wire::encode(&t);
        let msg = FromPolicyd::Table {
            sig: wire::sign(&key, &bytes),
            table: bytes,
        }
        .encode();
        let FromPolicyd::Table { table: got, sig } = FromPolicyd::decode(&msg).unwrap() else {
            panic!("not a table");
        };
        wire::verify(&key.verifying_key(), &got, &sig).unwrap();
        let back = wire::decode(&got).unwrap();
        assert_eq!(back.version, 42);
        let mut ctx = RequestCtx {
            principal: "agent:a",
            profile: "operator",
            grants: &[],
            capability: "click",
            app_id: None,
            title: None,
            class: Class::Private,
            app_trust: Trust::Standard,
            app_irreversible_capable: false,
            node: None,
            url: None,
            irreversible: Some("communication.send"),
            provenance: ProvenanceFacts {
                empty: false,
                trusts: [false; 4],
                sources: &[],
            },
        };
        assert_eq!(check(&back, &ctx).rule, "irreversible-prompts");
        ctx.irreversible = None;
        assert_eq!(check(&back, &ctx).outcome, Outcome::Allow);
        ctx.capability = "capture.window";
        ctx.class = Class::Secret;
        assert_eq!(check(&back, &ctx).outcome, Outcome::Deny);
    }
}
