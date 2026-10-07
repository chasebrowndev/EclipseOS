// SPDX-License-Identifier: AGPL-3.0-only
//! Sensitivity classification and app trust (S-05 §3, §6, §8). TCB.
//!
//! [`classify`] is the one function that produces a class for a target.
//! Class is a lattice **join** over every source that contributes, not a
//! first match: adding a rule can only raise, and rule order cannot change
//! the answer (S-05 §3).
//!
//! ```text
//! base  = public   if an owner `classify "public"` matcher hits
//!       = default  otherwise (`defaults { sensitivity }`, private unless set)
//! final = max(base, every `classify "private"/"secret"` matcher that hits,
//!             the caller's own raise)
//! ```
//!
//! Only owner policy can make something `public`; the caller's raise (the
//! compositor's sensitive set, `capture.redact-app-id`, a terminal's echo
//! off) can only push a class up.
//!
//! X11 windows are never `secret` (COMP-07 §2: the X server lets any X
//! client read another's input and pixels, so a `secret` promise cannot be
//! kept). A `secret` result for one is refused, reported, and the window
//! stays `private`; callers keep X11 windows out of `secret`-only paths.
//!
//! [`trust`] follows S-05 §6: owner `trust` rules only, `standard` by
//! default, `untrusted` when the surface has no `app_id`, and any
//! `untrusted` match beats a `trusted` one.

use regex::Regex;

use crate::check::Trust;
use crate::scope::{Class, Glob};

/// One matcher line inside a `classify` or `trust` node. Each line is its
/// own contributing source: any one that hits applies the node's class.
#[derive(Debug, Clone)]
pub enum Matcher {
    AppId(Vec<Glob>),
    Title(Regex),
    Url(Vec<Glob>),
    /// Output connector name.
    Output(Vec<Glob>),
    /// The principal that launched the window.
    LaunchedBy(Vec<Glob>),
    Xwayland(bool),
}

#[derive(Debug, Clone)]
pub struct ClassifyRule {
    pub class: Class,
    pub matchers: Vec<Matcher>,
}

#[derive(Debug, Clone)]
pub struct TrustRule {
    pub trust: Trust,
    pub matchers: Vec<Matcher>,
}

/// The classification half of the table.
#[derive(Debug, Clone)]
pub struct Classifier {
    /// `defaults { sensitivity }`: `private` or `secret`, never `public`.
    pub default: Class,
    pub classify: Vec<ClassifyRule>,
    pub trust: Vec<TrustRule>,
}

impl Default for Classifier {
    fn default() -> Self {
        Classifier {
            default: Class::Private,
            classify: Vec::new(),
            trust: Vec::new(),
        }
    }
}

/// What a target is, as far as a matcher can see (S-05 §3.1).
#[derive(Debug, Clone, Copy, Default)]
pub struct TargetFacts<'a> {
    pub app_id: Option<&'a str>,
    pub title: Option<&'a str>,
    pub url: Option<&'a str>,
    pub output: Option<&'a str>,
    pub launched_by: Option<&'a str>,
    pub xwayland: bool,
}

/// The classification and whether it was capped (an X11 window a rule tried
/// to make `secret`), so the caller can log the refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classified {
    pub class: Class,
    pub x11_secret_refused: bool,
}

fn any(globs: &[Glob], s: Option<&str>) -> bool {
    s.is_some_and(|s| globs.iter().any(|g| g.matches(s)))
}

impl Matcher {
    fn hits(&self, t: &TargetFacts<'_>) -> bool {
        match self {
            Matcher::AppId(g) => any(g, t.app_id),
            Matcher::Title(re) => t.title.is_some_and(|s| re.is_match(s)),
            Matcher::Url(g) => any(g, t.url),
            Matcher::Output(g) => any(g, t.output),
            Matcher::LaunchedBy(g) => any(g, t.launched_by),
            Matcher::Xwayland(b) => t.xwayland == *b,
        }
    }
}

/// S-05 §3. `raise` is the caller's own contribution; it can only raise.
pub fn classify(c: &Classifier, t: &TargetFacts<'_>, raise: Class) -> Classified {
    let hit = |r: &ClassifyRule| r.matchers.iter().any(|m| m.hits(t));
    let public = c.classify.iter().any(|r| r.class == Class::Public && hit(r));
    let base = if public { Class::Public } else { c.default };
    let raised = c
        .classify
        .iter()
        .filter(|r| r.class > Class::Public && hit(r))
        .map(|r| r.class)
        .fold(base, Class::max)
        .max(raise);
    if t.xwayland && raised == Class::Secret {
        return Classified {
            class: Class::Private,
            x11_secret_refused: true,
        };
    }
    Classified {
        class: raised,
        x11_secret_refused: false,
    }
}

/// S-05 §6.
pub fn trust(c: &Classifier, t: &TargetFacts<'_>) -> Trust {
    if t.app_id.is_none_or(str::is_empty) {
        return Trust::Untrusted;
    }
    let hit = |r: &TrustRule| r.matchers.iter().any(|m| m.hits(t));
    if c.trust.iter().any(|r| r.trust == Trust::Untrusted && hit(r)) {
        return Trust::Untrusted;
    }
    if c.trust.iter().any(|r| r.trust == Trust::Trusted && hit(r)) {
        return Trust::Trusted;
    }
    Trust::Standard
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(class: Class, m: Vec<Matcher>) -> ClassifyRule {
        ClassifyRule { class, matchers: m }
    }

    fn sample() -> Classifier {
        Classifier {
            default: Class::Private,
            classify: vec![
                rule(
                    Class::Secret,
                    vec![
                        Matcher::AppId(vec![Glob::new("org.keepassxc.KeePassXC"), Glob::new("bitwarden")]),
                        Matcher::Title(Regex::new("(?i)password|2fa").unwrap()),
                        Matcher::Url(vec![Glob::new("https://*.bank.example/*")]),
                    ],
                ),
                rule(
                    Class::Public,
                    vec![Matcher::Url(vec![Glob::new("https://*.wikipedia.org/*")])],
                ),
            ],
            trust: vec![
                TrustRule {
                    trust: Trust::Untrusted,
                    matchers: vec![
                        Matcher::AppId(vec![Glob::new("steam")]),
                        Matcher::Url(vec![Glob::new("http://*")]),
                    ],
                },
                TrustRule {
                    trust: Trust::Trusted,
                    matchers: vec![Matcher::AppId(vec![Glob::new("foot"), Glob::new("ec-*")])],
                },
            ],
        }
    }

    fn facts<'a>(app: &'a str, title: &'a str, url: Option<&'a str>) -> TargetFacts<'a> {
        TargetFacts {
            app_id: Some(app),
            title: Some(title),
            url,
            ..Default::default()
        }
    }

    #[test]
    fn default_public_and_raises() {
        let c = sample();
        let at = |f| classify(&c, &f, Class::Public).class;
        assert_eq!(at(facts("org.gnome.TextEditor", "notes", None)), Class::Private);
        assert_eq!(
            at(facts(
                "firefox",
                "Cats",
                Some("https://en.wikipedia.org/wiki/Cat")
            )),
            Class::Public
        );
        assert_eq!(
            at(facts("firefox", "Login", Some("https://my.bank.example/login"))),
            Class::Secret
        );
        assert_eq!(
            at(facts(
                "firefox",
                "Enter password",
                Some("https://en.wikipedia.org/x")
            )),
            Class::Secret,
            "a raise beats a public base"
        );
        assert_eq!(at(facts("bitwarden", "Vault", None)), Class::Secret);
    }

    #[test]
    fn the_caller_can_only_raise() {
        let c = sample();
        let wiki = facts("firefox", "Cats", Some("https://en.wikipedia.org/wiki/Cat"));
        assert_eq!(classify(&c, &wiki, Class::Secret).class, Class::Secret);
        assert_eq!(classify(&c, &wiki, Class::Public).class, Class::Public);
    }

    #[test]
    fn x11_is_never_secret_and_the_refusal_is_reported() {
        let c = sample();
        let mut f = facts("bitwarden", "Vault", None);
        f.xwayland = true;
        assert_eq!(
            classify(&c, &f, Class::Public),
            Classified {
                class: Class::Private,
                x11_secret_refused: true
            }
        );
    }

    #[test]
    fn trust_defaults_and_untrusted_wins() {
        let c = sample();
        assert_eq!(trust(&c, &facts("gimp", "x", None)), Trust::Standard);
        assert_eq!(trust(&c, &facts("foot", "x", None)), Trust::Trusted);
        assert_eq!(trust(&c, &facts("steam", "x", None)), Trust::Untrusted);
        assert_eq!(
            trust(&c, &facts("foot", "x", Some("http://evil"))),
            Trust::Untrusted,
            "untrusted beats trusted"
        );
        assert_eq!(trust(&c, &TargetFacts::default()), Trust::Untrusted, "no app_id");
    }

    /// S-05 §9 monotonicity: adding a raising rule never lowers a class.
    /// Order independence: shuffling the rules never changes one. A
    /// fixed-seed generator over a small fact space stands in for proptest.
    #[test]
    fn classification_is_monotone_and_order_independent() {
        let apps = ["a", "b", "keepass", "firefox"];
        let titles = ["x", "Password", "Inbox"];
        let urls = [
            None,
            Some("https://w.wikipedia.org/a"),
            Some("https://bank.example/x"),
        ];
        let pool = [
            rule(Class::Secret, vec![Matcher::AppId(vec![Glob::new("keepass")])]),
            rule(
                Class::Secret,
                vec![Matcher::Title(Regex::new("(?i)password").unwrap())],
            ),
            rule(
                Class::Public,
                vec![Matcher::Url(vec![Glob::new("https://*.wikipedia.org/*")])],
            ),
            rule(Class::Private, vec![Matcher::AppId(vec![Glob::new("firefox")])]),
            rule(
                Class::Secret,
                vec![Matcher::Url(vec![Glob::new("https://bank.example/*")])],
            ),
            rule(Class::Public, vec![Matcher::AppId(vec![Glob::new("a")])]),
        ];
        let mut seed: u64 = 7;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed as usize) % n
        };
        for _ in 0..300 {
            let mut base = Classifier::default();
            for _ in 0..next(5) {
                base.classify.push(pool[next(pool.len())].clone());
            }
            let mut more = base.clone();
            let extra = pool[next(pool.len())].clone();
            let raising = extra.class != Class::Public;
            more.classify.push(extra);
            let mut shuffled = base.clone();
            shuffled.classify.reverse();
            for app in apps {
                for title in titles {
                    for url in urls {
                        let f = facts(app, title, url);
                        let before = classify(&base, &f, Class::Public).class;
                        if raising {
                            assert!(classify(&more, &f, Class::Public).class >= before, "monotone");
                        }
                        assert_eq!(
                            classify(&shuffled, &f, Class::Public).class,
                            before,
                            "order independent"
                        );
                    }
                }
            }
        }
    }
}
