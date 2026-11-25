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
//! matches. Effects: `mute` (ignored like DND), `mute-until` (the same, until a
//! unix time in seconds), `quiet` (stored, no popup), `urgency`
//! (`low|normal|critical`) and `timeout-ms` (0 = server's maximum).
//!
//! The toast's Silence drawer writes this file through [`silence`] (ADR 0131).
//! It only ever tightens: a source already muted stays muted, a `mute-until`
//! is never shortened and nothing here clears a flag.
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
    /// Muted until this unix time (seconds); `None` for no timed mute.
    pub mute_until: Option<u64>,
    pub quiet: bool,
    pub urgency: Option<Urgency>,
    pub timeout_ms: Option<i32>,
}

impl Rule {
    /// Whether the source is muted at unix time `now`: for good, or until a
    /// time that has not come yet.
    pub(crate) fn muted(&self, now: u64) -> bool {
        self.mute || self.mute_until.is_some_and(|t| t > now)
    }
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
        mute_until: obj.get("mute-until").and_then(Value::as_u64),
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

/// How long, or how far, a source is silenced from the toast's drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Silence {
    /// Muted for an hour.
    Hour,
    /// Muted for a rolling 24 hours from now.
    Day,
    /// Muted until the human edits the file.
    Forever,
    /// No popup or sound, still stored in the history.
    BannersOnly,
}

/// Fold one silence into the rules text and return the new text. Pure, so the
/// ratchet is testable: it only sets `mute`, raises `mute-until` or sets
/// `quiet`, and keeps every other field and rule as it was. The rule edited is
/// the one `find` would pick for these names, else a new one is appended,
/// keyed on the desktop entry when there is one and on the app name if not.
pub(crate) fn apply_silence(
    text: &str,
    desktop_entry: &str,
    app_name: &str,
    how: Silence,
    now: u64,
) -> Option<String> {
    let de = norm(desktop_entry);
    let app = norm(app_name);
    if de.is_empty() && app.is_empty() {
        return None;
    }
    let mut top = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(o)) => o,
        // A missing or empty file starts one; a file that is not our shape is
        // left alone rather than overwritten.
        _ if text.trim().is_empty() => serde_json::Map::new(),
        _ => return None,
    };
    let list = top
        .entry("rules")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()?;
    let name = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(norm)
            .filter(|s| !s.is_empty())
    };
    let hit = |r: &Value| {
        let (rde, rapp) = (name(r, "desktop-entry"), name(r, "app"));
        if rde.is_some() {
            !de.is_empty() && rde.as_deref() == Some(&de) && (rapp.is_none() || rapp.as_deref() == Some(&app))
        } else {
            rapp.is_some() && rapp.as_deref() == Some(&app)
        }
    };
    // The same order `Rules::find` uses: the desktop entry first.
    let at = list
        .iter()
        .position(|r| name(r, "desktop-entry").is_some() && hit(r))
        .or_else(|| {
            list.iter()
                .position(|r| name(r, "desktop-entry").is_none() && hit(r))
        });
    let i = match at {
        Some(i) => i,
        None => {
            let mut o = serde_json::Map::new();
            if !de.is_empty() {
                o.insert("desktop-entry".into(), de.into());
            } else {
                o.insert("app".into(), app.into());
            }
            list.push(Value::Object(o));
            list.len() - 1
        }
    };
    let rule = list[i].as_object_mut()?;
    match how {
        Silence::Forever => {
            rule.insert("mute".into(), true.into());
            rule.remove("mute-until");
        }
        Silence::Hour | Silence::Day => {
            let until = match how {
                Silence::Hour => now + 3_600,
                _ => now + 86_400,
            };
            let forever = rule.get("mute").and_then(Value::as_bool).unwrap_or(false);
            let held = rule.get("mute-until").and_then(Value::as_u64).unwrap_or(0);
            if !forever {
                rule.insert("mute-until".into(), until.max(held).into());
            }
        }
        Silence::BannersOnly => {
            rule.insert("quiet".into(), true.into());
        }
    }
    serde_json::to_string_pretty(&Value::Object(top)).ok()
}

/// Silence a source (ADR 0131): read the rules file, tighten it, write it
/// back atomically (a sibling temp file renamed over it, 0600). The service
/// re-reads on the next arrival. `Err` carries the reason for the caller to
/// show; nothing is changed on failure.
pub fn silence(desktop_entry: &str, app_name: &str, how: Silence) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let out = apply_silence(&text, desktop_entry, app_name, how, now)
        .ok_or_else(|| std::io::Error::other("notification rules are not in a shape this can edit"))?;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no config directory"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(out.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, &path)
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

    fn rules_of(text: &str) -> Vec<Rule> {
        Rules::parse(text)
    }

    #[test]
    fn silencing_creates_a_rule_keyed_on_the_desktop_entry() {
        let t = apply_silence("", "Slack.desktop", "Slack", Silence::Forever, 100).unwrap();
        let r = rules_of(&t);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].desktop_entry.as_deref(), Some("slack"));
        assert!(r[0].mute && r[0].app.is_none());
        let t = apply_silence("", "", "Updater", Silence::BannersOnly, 100).unwrap();
        let r = rules_of(&t);
        assert_eq!(r[0].app.as_deref(), Some("updater"));
        assert!(r[0].quiet && !r[0].mute);
    }

    /// The ratchet: nothing a silence does loosens a rule, and what else the
    /// rule carries is kept.
    #[test]
    fn silencing_only_tightens_and_keeps_the_rest() {
        let base = r#"{"rules":[{"app":"Mail","urgency":"low","mute":true,"mute-until":50},{"app":"Other","quiet":true}]}"#;
        let t = apply_silence(base, "", "mail", Silence::Hour, 100).unwrap();
        let r = rules_of(&t);
        assert_eq!(r.len(), 2);
        assert!(r[0].mute, "a forever mute stays");
        assert_eq!(r[0].mute_until, Some(50), "a muted source's timer is left alone");
        assert_eq!(r[0].urgency, Some(Urgency::Low));
        assert!(r[1].quiet);
        // A timer is never shortened.
        let t = apply_silence(
            r#"{"rules":[{"app":"M","mute-until":9000}]}"#,
            "",
            "m",
            Silence::Hour,
            100,
        )
        .unwrap();
        assert_eq!(rules_of(&t)[0].mute_until, Some(9000));
        // Banners only sets quiet and leaves a mute alone.
        let t = apply_silence(
            r#"{"rules":[{"app":"M","mute":true}]}"#,
            "",
            "M",
            Silence::BannersOnly,
            1,
        )
        .unwrap();
        let r = rules_of(&t);
        assert!(r[0].mute && r[0].quiet);
    }

    #[test]
    fn timed_mutes_expire_and_forever_clears_the_timer() {
        let t = apply_silence("", "", "x", Silence::Hour, 1_000).unwrap();
        let r = &rules_of(&t)[0];
        assert!(r.muted(1_000) && r.muted(4_599) && !r.muted(4_600));
        let t = apply_silence(&t, "", "x", Silence::Day, 1_000).unwrap();
        let r = &rules_of(&t)[0];
        assert_eq!(r.mute_until, Some(1_000 + 86_400), "a rolling 24 h from now");
        let t = apply_silence(&t, "", "x", Silence::Forever, 1_000).unwrap();
        let r = &rules_of(&t)[0];
        assert!(r.mute && r.mute_until.is_none() && r.muted(u64::MAX));
    }

    #[test]
    fn a_file_that_is_not_ours_is_not_overwritten() {
        assert!(apply_silence("not json", "a", "b", Silence::Forever, 0).is_none());
        assert!(apply_silence("[]", "a", "b", Silence::Forever, 0).is_none());
        assert!(apply_silence("", "", "", Silence::Forever, 0).is_none());
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
