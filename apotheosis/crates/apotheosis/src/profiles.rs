// SPDX-License-Identifier: AGPL-3.0-only
//! Profiles (T-11 §6): choosing one for a pane, what a chosen profile starts the pane with,
//! and the automatic-switch bookkeeping.
//!
//! * Explicit choice (`launch profile=`, `pane.split profile=`, `--profile`, the default
//!   profile) applies the whole profile, inheritance resolved by `apoth-config`.
//! * Automatic choice scores each profile's `match` blocks additively: exact host 16,
//!   wildcard host 8, job 4, user 2, path 1. Every field a block names must match; a
//!   profile's score is its best block; ties go to file order. It is re-evaluated on
//!   `cwd-changed`, `host-changed`, `foreground-changed`.
//! * An automatic switch applies only the profile's `appearance`-class keys (T-10 §1.1),
//!   restores them on un-match, and never overrides a key the user changed at runtime in
//!   that pane ([`Switch`]).

use std::collections::HashSet;

use apoth_config::expr::{glob, is_wild};
use apoth_config::load::profile_values;
use apoth_config::model::{ProfileDecl, ProfileMatch};
use apoth_config::types::OverrideClass;
use apoth_config::{Config, Value};

/// What automatic matching looks at.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Context {
    pub(crate) host: Option<String>,
    pub(crate) user: Option<String>,
    /// Working directory (absolute path).
    pub(crate) path: Option<String>,
    /// Foreground job name.
    pub(crate) job: Option<String>,
}

fn field(pat: &Option<String>, val: &Option<String>) -> Option<bool> {
    pat.as_ref()
        .map(|p| val.as_deref().is_some_and(|v| glob(p, v)))
}

/// Score of one `match` block, `None` when it does not match.
pub(crate) fn score(m: &ProfileMatch, c: &Context) -> Option<u32> {
    let mut s = 0;
    if let Some(ok) = field(&m.host, &c.host) {
        if !ok {
            return None;
        }
        s += if m.host.as_deref().is_some_and(is_wild) {
            8
        } else {
            16
        };
    }
    if let Some(ok) = field(&m.job, &c.job) {
        if !ok {
            return None;
        }
        s += 4;
    }
    if let Some(ok) = field(&m.user, &c.user) {
        if !ok {
            return None;
        }
        s += 2;
    }
    if let Some(ok) = field(&m.path, &c.path) {
        if !ok {
            return None;
        }
        // T-11 §6 scores an exact path 1 and is silent on a wildcard path: it matches
        // and adds nothing (reported gap).
        if !m.path.as_deref().is_some_and(is_wild) {
            s += 1;
        }
    }
    Some(s)
}

/// The best-scoring profile for `c` (index into `profiles`), ties by file order.
pub(crate) fn choose<'a>(
    profiles: impl IntoIterator<Item = &'a ProfileDecl>,
    c: &Context,
) -> Option<usize> {
    let mut best: Option<(u32, usize)> = None;
    for (i, p) in profiles.into_iter().enumerate() {
        let Some(s) = p.matches.iter().filter_map(|m| score(m, c)).max() else {
            continue;
        };
        if best.is_none_or(|(b, _)| s > b) {
            best = Some((s, i));
        }
    }
    best.map(|(_, i)| i)
}

/// What a profile starts a pane with.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Spawn {
    pub(crate) program: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) env: Vec<(String, String)>,
}

/// The `shell`, `env` and `cwd` of a profile (with its ancestors).
pub(crate) fn spawn_of(cfg: &Config, name: &str) -> Option<Spawn> {
    cfg.profiles.iter().find(|p| p.value.name == name)?;
    let mut s = Spawn::default();
    for (k, v) in profile_values(cfg, name) {
        match (k.as_str(), v) {
            ("shell.program", Value::Str(p)) => s.program = Some(p),
            ("shell.args", Value::List(a)) => s.args = a,
            ("shell.cwd", Value::Str(p)) => s.cwd = Some(p),
            ("shell.env", Value::Map(m)) => {
                s.env = m
                    .into_iter()
                    .filter_map(|(k, v)| v.into_iter().next().map(|v| (k, v)))
                    .collect();
            }
            _ => {}
        }
    }
    Some(s)
}

/// True for keys an automatic switch may apply (`appearance` override class).
pub(crate) fn auto_key(path: &str) -> bool {
    apoth_config::schema::key(path).is_some_and(|k| k.class == OverrideClass::Appearance)
}

/// Per-pane automatic-switch state.
#[derive(Debug, Default, Clone)]
pub(crate) struct Switch {
    /// The automatically chosen profile in force.
    pub(crate) current: Option<String>,
    /// Keys the switch set, with the value each had before.
    applied: Vec<(String, Value)>,
    /// Keys the user changed at runtime in this pane: never touched again.
    touched: HashSet<String>,
}

impl Switch {
    /// The user changed `key` at runtime in this pane. HOOK(T-10 §5): called by the
    /// per-pane `config.set`, which is not built yet.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn touch(&mut self, key: &str) {
        self.touched.insert(key.to_string());
        self.applied.retain(|(k, _)| k != key);
    }

    /// Move to `next` (or to no profile). `now` reads a key's value in force in the pane.
    /// Returns the assignments to make: first restores of the old profile's keys the new
    /// one does not set, then the new profile's appearance keys. Sticky profiles stay until
    /// another profile matches.
    pub(crate) fn retarget(
        &mut self,
        cfg: &Config,
        next: Option<&str>,
        now: &dyn Fn(&str) -> Value,
    ) -> Vec<(String, Value)> {
        if self.current.as_deref() == next {
            return Vec::new();
        }
        if next.is_none()
            && self
                .current
                .as_deref()
                .and_then(|c| cfg.profiles.iter().find(|p| p.value.name == c))
                .is_some_and(|p| p.value.sticky)
        {
            return Vec::new();
        }
        let wanted: Vec<(String, Value)> = next
            .map(|n| {
                profile_values(cfg, n)
                    .into_iter()
                    .filter(|(k, _)| auto_key(k) && !self.touched.contains(k))
                    .collect()
            })
            .unwrap_or_default();
        let mut out = Vec::new();
        let old = std::mem::take(&mut self.applied);
        for (k, prev) in &old {
            if !wanted.iter().any(|(w, _)| w == k) {
                out.push((k.clone(), prev.clone()));
            }
        }
        for (k, v) in wanted {
            // The value before any profile: the remembered one when this key was already
            // switched, else the one in force now.
            let before = old
                .iter()
                .find(|(o, _)| *o == k)
                .map_or_else(|| now(&k), |(_, p)| p.clone());
            self.applied.push((k.clone(), before));
            out.push((k, v));
        }
        self.current = next.map(str::to_string);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_config::fs::MemFs;
    use apoth_config::load::{LoadInput, load};

    fn cfg(text: &str) -> Config {
        let fs = MemFs::new().with("/c/a.kdl", &format!("version 1\n{text}"));
        let r = load(
            &fs,
            &LoadInput {
                user: Some("/c/a.kdl".into()),
                ..LoadInput::default()
            },
            None,
        );
        assert!(
            r.diagnostics.iter().all(|d| !d.is_error()),
            "{:?}",
            r.diagnostics
        );
        r.config
    }

    fn ctx(host: &str, user: &str, path: &str, job: &str) -> Context {
        let o = |s: &str| (!s.is_empty()).then(|| s.to_string());
        Context {
            host: o(host),
            user: o(user),
            path: o(path),
            job: o(job),
        }
    }

    fn m(
        host: Option<&str>,
        user: Option<&str>,
        path: Option<&str>,
        job: Option<&str>,
    ) -> ProfileMatch {
        ProfileMatch {
            host: host.map(Into::into),
            user: user.map(Into::into),
            path: path.map(Into::into),
            job: job.map(Into::into),
        }
    }

    #[test]
    fn scores_are_additive() {
        let c = ctx("prod-1", "root", "/srv", "ssh");
        assert_eq!(score(&m(Some("prod-1"), None, None, None), &c), Some(16));
        assert_eq!(score(&m(Some("prod-*"), None, None, None), &c), Some(8));
        assert_eq!(score(&m(None, None, None, Some("ssh")), &c), Some(4));
        assert_eq!(score(&m(None, Some("root"), None, None), &c), Some(2));
        assert_eq!(score(&m(None, None, Some("/srv"), None), &c), Some(1));
        assert_eq!(score(&m(None, None, Some("/s*"), None), &c), Some(0));
        assert_eq!(
            score(
                &m(Some("prod-1"), Some("root"), Some("/srv"), Some("ssh")),
                &c
            ),
            Some(23)
        );
        // Every named field must match.
        assert_eq!(
            score(&m(Some("prod-1"), Some("alice"), None, None), &c),
            None
        );
        assert_eq!(
            score(&m(None, None, None, Some("vim")), &ctx("", "", "", "")),
            None
        );
    }

    #[test]
    fn choose_takes_the_best_and_ties_by_file_order() {
        let c = cfg(r#"
profile "wild" { match host="prod-*"; }
profile "exact" { match host="prod-1"; }
profile "job-user" { match job="ssh" user="root"; }
profile "tie" { match job="ssh" user="root"; }
profile "never" { match host="other"; }
"#);
        let ps: Vec<&ProfileDecl> = c.profiles.iter().map(|p| &p.value).collect();
        let pick = |cx: &Context| choose(ps.iter().copied(), cx).map(|i| ps[i].name.as_str());
        assert_eq!(pick(&ctx("prod-1", "", "", "")), Some("exact"));
        assert_eq!(pick(&ctx("prod-2", "", "", "")), Some("wild"));
        // 4 + 2 = 6 beats nothing; the earlier of two equal scores wins.
        assert_eq!(pick(&ctx("", "root", "", "ssh")), Some("job-user"));
        // Host wildcard (8) beats job+user (6).
        assert_eq!(pick(&ctx("prod-9", "root", "", "ssh")), Some("wild"));
        assert_eq!(pick(&ctx("laptop", "me", "/", "zsh")), None);
    }

    #[test]
    fn spawn_follows_inheritance() {
        let c = cfg(r#"
profile "base" {
    shell { program "/bin/zsh"; args "-l"; }
    env { EDITOR "vi"; }
}
profile "work" {
    inherits "base"
    cwd "/srv"
}
"#);
        let s = spawn_of(&c, "work").unwrap();
        assert_eq!(s.program.as_deref(), Some("/bin/zsh"));
        assert_eq!(s.args, vec!["-l".to_string()]);
        assert_eq!(s.cwd.as_deref(), Some("/srv"));
        assert_eq!(s.env, vec![("EDITOR".to_string(), "vi".to_string())]);
        assert!(spawn_of(&c, "missing").is_none());
    }

    #[test]
    fn auto_switch_applies_appearance_only_restores_and_respects_runtime_changes() {
        let c = cfg(r##"
profile "red" {
    match host="prod-*"
    colors { background "#330000"; }
    cursor { style "bar"; }
    shell { program "/bin/sh"; }
    scrollback { lines 10; }
}
profile "blue" {
    match host="dev-*"
    colors { background "#000033"; }
}
"##);
        assert!(auto_key("colors.background"));
        assert!(!auto_key("shell.program"), "security keys never switch");
        let base = |k: &str| Value::Str(format!("base:{k}"));
        let mut s = Switch::default();
        let set = s.retarget(&c, Some("red"), &base);
        let keys: Vec<&str> = set.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"colors.background"));
        assert!(!keys.contains(&"shell.program"));
        assert!(
            !keys.iter().any(|k| k.starts_with("scrollback")),
            "behaviour keys never switch"
        );
        // Same profile again: nothing.
        assert!(s.retarget(&c, Some("red"), &base).is_empty());
        // The user changes the cursor in this pane; moving to blue restores the background
        // only through blue's own value and leaves the cursor alone.
        s.touch("cursor.style");
        let set = s.retarget(&c, Some("blue"), &base);
        assert!(set.iter().all(|(k, _)| k != "cursor.style"));
        assert!(set.contains(&(
            "colors.background".to_string(),
            Value::Color([0, 0, 0x33, 0xff])
        )));
        // Un-match restores the value from before any profile.
        let set = s.retarget(&c, None, &base);
        assert_eq!(
            set,
            vec![(
                "colors.background".to_string(),
                Value::Str("base:colors.background".into())
            )]
        );
        assert_eq!(s.current, None);
    }

    #[test]
    fn sticky_profiles_stay_on_unmatch() {
        let c = cfg(
            r##"profile "s" { sticky #true; match host="h"; colors { background "#111111"; }; }"##,
        );
        let base = |_: &str| Value::Null;
        let mut s = Switch::default();
        assert!(!s.retarget(&c, Some("s"), &base).is_empty());
        assert!(s.retarget(&c, None, &base).is_empty());
        assert_eq!(s.current.as_deref(), Some("s"));
    }
}
