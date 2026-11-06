// SPDX-License-Identifier: AGPL-3.0-only
//! Per-app rules, read from `$XDG_CONFIG_HOME/eclipse/notifications.json`
//! (else `~/.config/eclipse/notifications.json`):
//!
//! ```json
//! {"rules": [
//!   {"desktop-entry": "slack", "quiet": true},
//!   {"app": "Updater", "mute": true},
//!   {"app": "Mail", "urgency": "low", "timeout-ms": 3000}
//! ]}
//! ```
//!
//! A rule matches on `desktop-entry` (the hint, case-insensitive, no
//! `.desktop`) and/or `app` (the `app_name`, case-insensitive). The first rule
//! whose `desktop-entry` matches wins; failing that, the first whose `app`
//! matches. A rule naming both must match both. A rule naming neither never
//! matches. Effects: `mute` (ignored like DND), `quiet` (stored, no popup),
//! `urgency` (`low|normal|critical`) and `timeout-ms` (0 = server's maximum).
//!
//! Missing, unreadable or malformed files mean no rules; a bad rule is
//! skipped alone. Rules can lower urgency; they never bypass the mode.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use super::Urgency;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Rule {
    pub desktop_entry: Option<String>,
    pub app: Option<String>,
    pub mute: bool,
    pub quiet: bool,
    pub urgency: Option<Urgency>,
    pub timeout_ms: Option<i32>,
}

#[derive(Debug, Default)]
pub(crate) struct Rules {
    rules: Vec<Rule>,
    path: Option<PathBuf>,
    stamp: Option<SystemTime>,
}

pub(crate) fn path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("eclipse").join("notifications.json"))
}

fn norm(s: &str) -> String {
    let s = s.trim().to_lowercase();
    s.strip_suffix(".desktop").map_or(s.clone(), str::to_owned)
}

fn parse_rule(v: &Value) -> Option<Rule> {
    let obj = v.as_object()?;
    let text = |k: &str| {
        obj.get(k)
            .and_then(Value::as_str)
            .map(norm)
            .filter(|s| !s.is_empty())
    };
    let flag = |k: &str| obj.get(k).and_then(Value::as_bool).unwrap_or(false);
    let rule = Rule {
        desktop_entry: text("desktop-entry"),
        app: text("app"),
        mute: flag("mute"),
        quiet: flag("quiet"),
        urgency: obj
            .get("urgency")
            .and_then(Value::as_str)
            .and_then(Urgency::parse),
        timeout_ms: obj
            .get("timeout-ms")
            .and_then(Value::as_i64)
            .map(|ms| ms.clamp(0, i64::from(i32::MAX)) as i32),
    };
    (rule.desktop_entry.is_some() || rule.app.is_some()).then_some(rule)
}

impl Rules {
    pub(crate) fn parse(text: &str) -> Vec<Rule> {
        let Ok(Value::Object(top)) = serde_json::from_str::<Value>(text) else {
            return Vec::new();
        };
        top.get("rules")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(parse_rule).collect())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn from_rules(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            ..Self::default()
        }
    }

    /// Rules read from the standard path now, re-read when the file changes.
    pub(crate) fn load_default() -> Self {
        let mut r = Self {
            path: path(),
            ..Self::default()
        };
        r.refresh();
        r
    }

    /// Re-read the file if its mtime moved. A stat per notification.
    pub(crate) fn refresh(&mut self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if stamp == self.stamp {
            return;
        }
        self.stamp = stamp;
        self.rules = read(path);
    }

    /// The rule for a notification, if any.
    pub(crate) fn find(&self, desktop_entry: &str, app_name: &str) -> Option<&Rule> {
        let de = norm(desktop_entry);
        let app = norm(app_name);
        let de_match = |r: &Rule| !de.is_empty() && r.desktop_entry.as_deref() == Some(&de);
        let app_match = |r: &Rule| r.app.as_deref() == Some(&app);
        self.rules
            .iter()
            // A rule naming both must match both.
            .find(|r| de_match(r) && (r.app.is_none() || app_match(r)))
            .or_else(|| {
                self.rules
                    .iter()
                    .find(|r| r.desktop_entry.is_none() && app_match(r))
            })
    }
}

fn read(path: &Path) -> Vec<Rule> {
    std::fs::read_to_string(path)
        .map(|t| Rules::parse(&t))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rules_and_skips_bad_ones() {
        let r = Rules::parse(
            r#"{"rules":[
                {"desktop-entry":"Slack.desktop","quiet":true},
                {"mute":true},
                {"app":"Mail","urgency":"low","timeout-ms":3000},
                {"app":"X","urgency":"loud"},
                7
            ]}"#,
        );
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].desktop_entry.as_deref(), Some("slack"));
        assert!(r[0].quiet);
        assert_eq!(r[1].urgency, Some(Urgency::Low));
        assert_eq!(r[1].timeout_ms, Some(3000));
        assert_eq!(r[2].urgency, None);
        assert!(Rules::parse("not json").is_empty());
        assert!(Rules::parse("{}").is_empty());
    }

    #[test]
    fn desktop_entry_beats_app_name() {
        let rules = Rules::from_rules(Rules::parse(
            r#"{"rules":[{"app":"Chat","mute":true},{"desktop-entry":"chat","quiet":true}]}"#,
        ));
        let r = rules.find("chat", "Chat").unwrap();
        assert!(r.quiet && !r.mute);
        // No desktop entry on the notification: fall back to the app name.
        let r = rules.find("", "CHAT").unwrap();
        assert!(r.mute);
        assert!(rules.find("other", "Other").is_none());
    }

    #[test]
    fn a_rule_naming_both_needs_both() {
        let rules = Rules::from_rules(Rules::parse(
            r#"{"rules":[{"desktop-entry":"a","app":"B","mute":true}]}"#,
        ));
        assert!(rules.find("a", "B").is_some());
        assert!(rules.find("a", "C").is_none());
        assert!(rules.find("", "B").is_none());
    }

    #[test]
    fn missing_file_is_no_rules() {
        let mut r = Rules {
            path: Some("/nonexistent/ec/notifications.json".into()),
            ..Rules::default()
        };
        r.refresh();
        assert!(r.find("a", "b").is_none());
    }
}
