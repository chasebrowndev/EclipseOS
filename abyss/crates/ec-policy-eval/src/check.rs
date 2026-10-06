// SPDX-License-Identifier: AGPL-3.0-only
//! The enforcement check (COMP-11 §3, S-02 §3–§4, ADR 0008). TCB.
//!
//! [`check`] evaluates one request against a compiled [`Table`] and returns
//! one of `Allow | Deny | Prompt | Defer`, with the id of the rule that
//! produced it. It is called at step 7 of the enforcement order (COMP-08
//! §10): after the grant, scope, rate and sensitivity checks, and before any
//! state mutation. An `Allow` here is necessary, never sufficient: the grant
//! must already permit the request.
//!
//! Properties the code holds, not conventions:
//!
//! - **Fail-closed.** A request no rule matches is denied. An empty table
//!   denies everything. A predicate this evaluator cannot answer is not
//!   representable: the compiler must reject the rule instead.
//! - **Phased and monotone** (S-02 §3). All `deny` rules, then `prompt`, then
//!   `defer`, then `allow`; the first match in the first matching phase wins.
//!   Adding an `allow` rule can never change a deny-phase result.
//! - **Defer only tightens** (COMP-11 §5). [`DeferAnswer`] has no variant
//!   that grants. `Fallthrough` reaches the static `allow` phase and nothing
//!   broader; no answer at all (timeout, `policyd` gone) is a denial.
//! - **Hard rules first** (S-02 §2). They run before the table, so no table
//!   can relax them.
//! - **No allocation, no I/O, no blocking.** [`RequestCtx`] borrows every
//!   fact; [`Decision`] borrows its rule id from the table.
//!
//! Not yet represented (S-02 §3), so a rule using them cannot be compiled to
//! this table: `time`, `rate_gt`, `grant_scope`, `target_handle`,
//! `provenance_min_trust`/`_head`/`_age_gt`/`_mismatch`, `egress_bytes_gt`,
//! `secret_bound_host_mismatch` and `node_credential`.

use regex::Regex;

use crate::scope::{Class, Glob};

/// What the check decides (COMP-11 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allow,
    Deny,
    Prompt,
    Defer,
}

/// An outcome and the id of the rule that produced it, which the audit
/// record carries (S-02 §1, "every decision cites the rule id").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision<'a> {
    pub outcome: Outcome,
    pub rule: &'a str,
}

/// No rule matched. The only route to it is the end of [`check`].
pub const NO_MATCH: &str = "default-deny";
/// `system.policy` and `system.firmware` (S-02 §2).
pub const HARD_SYSTEM_TAXONOMY: &str = "hard:system-taxonomy";
/// `policy.edit` is never granted to an agent (S-02 §2).
pub const HARD_POLICY_EDIT: &str = "hard:policy-edit";
/// A deferral that got no answer (COMP-11 §5 step 4).
pub const DEFERRED_TIMEOUT: &str = "deferred_timeout";

/// App and provenance trust, lowest first (S-05, S-02 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Trust {
    Untrusted,
    Standard,
    Trusted,
    Human,
}

/// One grant fact an `unless` clause can test (S-02 §3: `unless` is limited
/// to grant facts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantFact<'a> {
    pub capability: &'a str,
    pub unattended: bool,
}

/// The node the request acts on, from the semantic tree or vision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeFacts<'a> {
    pub role: &'a str,
    pub name: &'a str,
    pub source: &'a str,
    pub confidence: f32,
}

/// The request's provenance tag, summarised (S-07 §5). `empty` is an act
/// with no asserted input (`provenance_absent`, A-06 §10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProvenanceFacts<'a> {
    pub empty: bool,
    /// Indexed by [`Trust`]: whether any link carries that trust.
    pub trusts: [bool; 4],
    /// The source kinds present (`channel`, `url`, ...).
    pub sources: &'a [&'a str],
}

/// Everything the check may read, all of it facts the compositor already
/// has (COMP-11 §3). Borrowed: building one allocates nothing.
#[derive(Debug, Clone, Copy)]
pub struct RequestCtx<'a> {
    pub principal: &'a str,
    pub profile: &'a str,
    pub grants: &'a [GrantFact<'a>],
    pub capability: &'a str,
    pub app_id: Option<&'a str>,
    pub title: Option<&'a str>,
    pub class: Class,
    pub app_trust: Trust,
    pub app_irreversible_capable: bool,
    pub node: Option<NodeFacts<'a>>,
    pub url: Option<&'a str>,
    /// The irreversible taxonomy id this request matched, if any (S-06).
    pub irreversible: Option<&'a str>,
    pub provenance: ProvenanceFacts<'a>,
}

/// One compiled predicate (S-02 §3). Every predicate is a conjunct; a list
/// inside one is a disjunction (`capability "a" "b"` is a or b).
#[derive(Debug, Clone)]
pub enum Pred {
    Capability(Vec<Glob>),
    TargetClass(Vec<Class>),
    TargetAppId(Vec<Glob>),
    Title(Regex),
    NodeRole(Vec<String>),
    NodeName(Regex),
    NodeSource(Vec<String>),
    NodeConfidenceLt(f32),
    /// Taxonomy id glob; `*` matches any matched taxonomy, never none.
    Irreversible(Vec<Glob>),
    Url(Vec<Glob>),
    Principal(Vec<Glob>),
    PrincipalProfile(Vec<String>),
    AppTrust(Vec<Trust>),
    AppIrreversibleCapable(bool),
    ProvenanceContainsTrust(Trust),
    ProvenanceContainsSource(String),
    ProvenanceAbsent,
}

/// `unless grant_has "<cap>" [unattended=true]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unless {
    pub capability: String,
    pub unattended: bool,
}

#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub id: String,
    pub preds: Vec<Pred>,
    pub unless: Vec<Unless>,
}

/// The rules, by phase (S-02 §4). Order within a phase is source order.
#[derive(Debug, Clone, Default)]
pub struct Phases {
    pub deny: Vec<CompiledRule>,
    pub prompt: Vec<CompiledRule>,
    pub defer: Vec<CompiledRule>,
    pub allow: Vec<CompiledRule>,
}

/// The enforcement table the compositor evaluates in-process. `Default` is
/// the empty table, which denies everything.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub version: u64,
    pub rules: Phases,
}

/// What `policyd` may answer a deferral with (COMP-11 §5). There is no
/// `Allow`: a compromised `policyd` can deny, never widen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferAnswer {
    Deny,
    Prompt,
    Fallthrough,
}

fn any_glob(globs: &[Glob], s: &str) -> bool {
    globs.iter().any(|g| g.matches(s))
}

impl Pred {
    fn matches(&self, ctx: &RequestCtx<'_>) -> bool {
        match self {
            Pred::Capability(g) => any_glob(g, ctx.capability),
            Pred::TargetClass(c) => c.contains(&ctx.class),
            // A request with no target app matches no app predicate.
            Pred::TargetAppId(g) => ctx.app_id.is_some_and(|a| any_glob(g, a)),
            Pred::Title(re) => ctx.title.is_some_and(|t| re.is_match(t)),
            Pred::NodeRole(r) => ctx.node.is_some_and(|n| r.iter().any(|r| r == n.role)),
            Pred::NodeName(re) => ctx.node.is_some_and(|n| re.is_match(n.name)),
            Pred::NodeSource(s) => ctx.node.is_some_and(|n| s.iter().any(|s| s == n.source)),
            Pred::NodeConfidenceLt(v) => ctx.node.is_some_and(|n| n.confidence < *v),
            Pred::Irreversible(g) => ctx.irreversible.is_some_and(|t| any_glob(g, t)),
            Pred::Url(g) => ctx.url.is_some_and(|u| any_glob(g, u)),
            Pred::Principal(g) => any_glob(g, ctx.principal),
            Pred::PrincipalProfile(p) => p.iter().any(|p| p == ctx.profile),
            Pred::AppTrust(t) => t.contains(&ctx.app_trust),
            Pred::AppIrreversibleCapable(b) => ctx.app_irreversible_capable == *b,
            Pred::ProvenanceContainsTrust(t) => ctx.provenance.trusts[*t as usize],
            Pred::ProvenanceContainsSource(s) => ctx.provenance.sources.iter().any(|k| k == s),
            Pred::ProvenanceAbsent => ctx.provenance.empty,
        }
    }
}

impl Unless {
    fn holds(&self, ctx: &RequestCtx<'_>) -> bool {
        ctx.grants
            .iter()
            .any(|g| g.capability == self.capability && (!self.unattended || g.unattended))
    }
}

impl CompiledRule {
    fn matches(&self, ctx: &RequestCtx<'_>) -> bool {
        self.preds.iter().all(|p| p.matches(ctx)) && !self.unless.iter().any(|u| u.holds(ctx))
    }
}

fn first<'a>(rules: &'a [CompiledRule], ctx: &RequestCtx<'_>) -> Option<&'a str> {
    rules.iter().find(|r| r.matches(ctx)).map(|r| r.id.as_str())
}

fn is_system(principal: &str) -> bool {
    principal.starts_with("system:")
}

/// The S-02 §2 hard rules that a request alone can decide. They precede the
/// table, so no rule can relax them.
fn hard(ctx: &RequestCtx<'_>) -> Option<Decision<'static>> {
    if is_system(ctx.principal) {
        return None;
    }
    if ctx
        .irreversible
        .is_some_and(|t| t == "system.policy" || t == "system.firmware")
    {
        return Some(Decision {
            outcome: Outcome::Deny,
            rule: HARD_SYSTEM_TAXONOMY,
        });
    }
    if ctx.capability == "policy.edit" {
        return Some(Decision {
            outcome: Outcome::Deny,
            rule: HARD_POLICY_EDIT,
        });
    }
    None
}

/// Evaluate one request (COMP-11 §3). Hard rules, then deny → prompt →
/// defer → allow, first match wins; nothing matching is a denial.
pub fn check<'a>(table: &'a Table, ctx: &RequestCtx<'_>) -> Decision<'a> {
    if let Some(d) = hard(ctx) {
        return d;
    }
    let phases = [
        (&table.rules.deny, Outcome::Deny),
        (&table.rules.prompt, Outcome::Prompt),
        (&table.rules.defer, Outcome::Defer),
        (&table.rules.allow, Outcome::Allow),
    ];
    for (rules, outcome) in phases {
        if let Some(rule) = first(rules, ctx) {
            return Decision { outcome, rule };
        }
    }
    Decision {
        outcome: Outcome::Deny,
        rule: NO_MATCH,
    }
}

/// Resolve a [`check`] that returned `Defer` from `rule`, given `policyd`'s
/// answer, or `None` when it timed out or is gone (COMP-11 §5).
/// `Fallthrough` evaluates the static `allow` phase only, so the result is
/// never broader than the table already permitted.
pub fn after_defer<'a>(
    table: &'a Table,
    ctx: &RequestCtx<'_>,
    rule: &'a str,
    answer: Option<DeferAnswer>,
) -> Decision<'a> {
    match answer {
        None => Decision {
            outcome: Outcome::Deny,
            rule: DEFERRED_TIMEOUT,
        },
        Some(DeferAnswer::Deny) => Decision {
            outcome: Outcome::Deny,
            rule,
        },
        Some(DeferAnswer::Prompt) => Decision {
            outcome: Outcome::Prompt,
            rule,
        },
        Some(DeferAnswer::Fallthrough) => match first(&table.rules.allow, ctx) {
            Some(rule) => Decision {
                outcome: Outcome::Allow,
                rule,
            },
            None => Decision {
                outcome: Outcome::Deny,
                rule: NO_MATCH,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(capability: &'a str) -> RequestCtx<'a> {
        RequestCtx {
            principal: "agent:test",
            profile: "operator",
            grants: &[],
            capability,
            app_id: None,
            title: None,
            class: Class::Private,
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
        }
    }

    fn rule(id: &str, preds: Vec<Pred>) -> CompiledRule {
        CompiledRule {
            id: id.to_owned(),
            preds,
            unless: Vec::new(),
        }
    }

    fn caps(c: &[&str]) -> Pred {
        Pred::Capability(c.iter().map(|c| Glob::new(c)).collect())
    }

    fn default_allow() -> CompiledRule {
        rule("default-allow", vec![caps(&["*"])])
    }

    #[test]
    fn the_empty_table_denies_everything() {
        let t = Table::default();
        for cap in ["scene.list", "seat.key", "capture.output", ""] {
            assert_eq!(
                check(&t, &ctx(cap)),
                Decision {
                    outcome: Outcome::Deny,
                    rule: NO_MATCH
                }
            );
        }
    }

    #[test]
    fn deny_beats_prompt_beats_defer_beats_allow() {
        let mut t = Table::default();
        t.rules.allow.push(default_allow());
        t.rules.defer.push(rule("d", vec![caps(&["seat.*"])]));
        t.rules
            .prompt
            .push(rule("p", vec![caps(&["seat.key", "seat.text"])]));
        t.rules.deny.push(rule("x", vec![caps(&["seat.key"])]));
        let at = |c| check(&t, &ctx(c));
        assert_eq!(at("seat.key").outcome, Outcome::Deny);
        assert_eq!(at("seat.key").rule, "x");
        assert_eq!(at("seat.text").outcome, Outcome::Prompt);
        assert_eq!(at("seat.pointer").outcome, Outcome::Defer);
        assert_eq!(at("scene.list").outcome, Outcome::Allow);
        assert_eq!(at("scene.list").rule, "default-allow");
    }

    #[test]
    fn the_first_match_in_a_phase_wins() {
        let mut t = Table::default();
        t.rules.prompt.push(rule("one", vec![caps(&["click"])]));
        t.rules.prompt.push(rule("two", vec![caps(&["*"])]));
        assert_eq!(check(&t, &ctx("click")).rule, "one");
        assert_eq!(check(&t, &ctx("seat.key")).rule, "two");
    }

    #[test]
    fn predicates_are_a_conjunction_and_a_missing_fact_never_matches() {
        let mut t = Table::default();
        t.rules.deny.push(rule(
            "no-secret-capture",
            vec![caps(&["capture.*"]), Pred::TargetClass(vec![Class::Secret])],
        ));
        t.rules.deny.push(rule(
            "shell",
            vec![caps(&["seat.text"]), Pred::TargetAppId(vec![Glob::new("foot")])],
        ));
        t.rules.allow.push(default_allow());
        let mut c = ctx("capture.window");
        assert_eq!(check(&t, &c).outcome, Outcome::Allow);
        c.class = Class::Secret;
        assert_eq!(check(&t, &c).rule, "no-secret-capture");
        // No target app: the app predicate cannot be satisfied.
        assert_eq!(check(&t, &ctx("seat.text")).outcome, Outcome::Allow);
        let mut c = ctx("seat.text");
        c.app_id = Some("foot");
        assert_eq!(check(&t, &c).rule, "shell");
    }

    #[test]
    fn unless_relaxes_only_on_the_named_grant_fact() {
        let mut t = Table::default();
        let mut r = rule("no-secret-capture", vec![caps(&["capture.*"])]);
        r.unless.push(Unless {
            capability: "capture.secret".into(),
            unattended: true,
        });
        t.rules.deny.push(r);
        let attended = [GrantFact {
            capability: "capture.secret",
            unattended: false,
        }];
        let unattended = [GrantFact {
            capability: "capture.secret",
            unattended: true,
        }];
        let mut c = ctx("capture.output");
        assert_eq!(check(&t, &c).outcome, Outcome::Deny);
        c.grants = &attended;
        assert_eq!(check(&t, &c).outcome, Outcome::Deny);
        c.grants = &unattended;
        // Relaxed out of the deny phase, and nothing allows: still denied.
        assert_eq!(
            check(&t, &c),
            Decision {
                outcome: Outcome::Deny,
                rule: NO_MATCH
            }
        );
    }

    #[test]
    fn hard_rules_precede_every_table() {
        let mut t = Table::default();
        t.rules.allow.push(default_allow());
        let mut c = ctx("seat.action");
        c.irreversible = Some("system.firmware");
        assert_eq!(check(&t, &c).rule, HARD_SYSTEM_TAXONOMY);
        assert_eq!(check(&t, &ctx("policy.edit")).rule, HARD_POLICY_EDIT);
        // A system principal is not an agent.
        c.principal = "system:updater";
        assert_eq!(check(&t, &c).outcome, Outcome::Allow);
    }

    #[test]
    fn irreversible_star_needs_a_match() {
        let mut t = Table::default();
        t.rules.prompt.push(rule(
            "irreversible-prompts",
            vec![caps(&["click"]), Pred::Irreversible(vec![Glob::new("*")])],
        ));
        assert_eq!(check(&t, &ctx("click")).outcome, Outcome::Deny);
        let mut c = ctx("click");
        c.irreversible = Some("communication.send");
        assert_eq!(check(&t, &c).outcome, Outcome::Prompt);
    }

    #[test]
    fn provenance_and_node_predicates() {
        let mut t = Table::default();
        t.rules.defer.push(rule(
            "laundering",
            vec![Pred::ProvenanceContainsSource("channel".into())],
        ));
        t.rules.prompt.push(rule(
            "vision",
            vec![
                Pred::NodeSource(vec!["vision".into()]),
                Pred::NodeConfidenceLt(0.9),
            ],
        ));
        t.rules.prompt.push(rule("blind", vec![Pred::ProvenanceAbsent]));
        let sources = ["channel"];
        let mut c = ctx("seat.action");
        c.provenance.sources = &sources;
        assert_eq!(check(&t, &c).rule, "laundering");
        let mut c = ctx("click");
        c.node = Some(NodeFacts {
            role: "button",
            name: "Send",
            source: "vision",
            confidence: 0.5,
        });
        assert_eq!(check(&t, &c).rule, "vision");
        let mut c = ctx("click");
        c.provenance.empty = true;
        assert_eq!(check(&t, &c).rule, "blind");
    }

    #[test]
    fn defer_never_widens() {
        let mut t = Table::default();
        t.rules.defer.push(rule("d", vec![caps(&["seat.*"])]));
        let c = ctx("seat.text");
        let d = check(&t, &c);
        assert_eq!(d.outcome, Outcome::Defer);
        // No allow rule: fallthrough still denies.
        for answer in [
            None,
            Some(DeferAnswer::Deny),
            Some(DeferAnswer::Prompt),
            Some(DeferAnswer::Fallthrough),
        ] {
            assert_ne!(after_defer(&t, &c, d.rule, answer).outcome, Outcome::Allow);
        }
        assert_eq!(after_defer(&t, &c, d.rule, None).rule, DEFERRED_TIMEOUT);
        t.rules.allow.push(default_allow());
        assert_eq!(
            after_defer(&t, &c, "d", Some(DeferAnswer::Fallthrough)).outcome,
            Outcome::Allow
        );
        assert_eq!(after_defer(&t, &c, "d", None).outcome, Outcome::Deny);
    }

    /// S-02 §7: adding allow rules never changes a decision that a deny,
    /// prompt or defer rule made; it can only turn a no-match denial into an
    /// allow. A fixed-seed generator stands in for a property-test crate.
    #[test]
    fn adding_allow_rules_only_ever_answers_a_no_match() {
        let capsets: [&[&str]; 5] = [
            &["*"],
            &["seat.*"],
            &["seat.key"],
            &["capture.*"],
            &["click", "seat.text"],
        ];
        let requests = ["seat.key", "seat.text", "click", "capture.output", "scene.list"];
        let mut seed: u64 = 0x5eed;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % n
        };
        for _ in 0..500 {
            let mut t = Table::default();
            for phase in 0..3 {
                for i in 0..next(3) {
                    let r = rule(&format!("r{phase}.{i}"), vec![caps(capsets[next(5)])]);
                    match phase {
                        0 => t.rules.deny.push(r),
                        1 => t.rules.prompt.push(r),
                        _ => t.rules.defer.push(r),
                    }
                }
            }
            let before: Vec<(Outcome, String)> = requests
                .iter()
                .map(|r| {
                    let d = check(&t, &ctx(r));
                    (d.outcome, d.rule.to_owned())
                })
                .collect();
            for i in 0..1 + next(4) {
                t.rules
                    .allow
                    .push(rule(&format!("a{i}"), vec![caps(capsets[next(5)])]));
            }
            for (r, (outcome, rule)) in requests.iter().zip(before) {
                let now = check(&t, &ctx(r));
                if rule == NO_MATCH {
                    assert!(matches!(now.outcome, Outcome::Deny | Outcome::Allow));
                } else {
                    assert_eq!((now.outcome, now.rule), (outcome, rule.as_str()), "{r}");
                }
            }
        }
    }
}
