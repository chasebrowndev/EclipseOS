// SPDX-License-Identifier: AGPL-3.0-only

//! App-identity resolution (D-08 §4) as a pure function over injected facts.
//!
//! The result is advisory: it is shown to the user and may only narrow a
//! decision. It is never an enforcement key (ADR 0096).

use std::collections::HashMap;

use ecl_portal_proto::{IdentitySource, Requester};

/// Tag set by a `wp_security_context` sandbox engine, as relayed to us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityContextTag {
    pub app_id: String,
    pub engine: Option<String>,
}

/// Everything the daemon could learn about the caller, collected by the edge
/// code. Any field may be absent.
#[derive(Debug, Default)]
pub struct Facts<'a> {
    /// Text of the caller's `/.flatpak-info`.
    pub flatpak_info: Option<&'a str>,
    pub security_context: Option<&'a SecurityContextTag>,
    /// The systemd unit name, when known directly.
    pub unit: Option<&'a str>,
    /// Text of `/proc/<pid>/cgroup`.
    pub cgroup: Option<&'a str>,
    /// Basename of the caller's executable (or `comm`).
    pub exe_name: Option<&'a str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopEntry {
    pub name: Option<String>,
    pub icon: Option<String>,
    pub startup_wm_class: Option<String>,
    /// Basename of the first `Exec` word.
    pub exec_base: Option<String>,
    /// The raw `Exec` line (field codes unexpanded).
    pub exec: Option<String>,
    /// `Terminal=true`: needs a terminal; the portal does not launch these.
    pub terminal: bool,
}

/// `.desktop` ids (without the suffix) to their entries.
#[derive(Debug, Clone, Default)]
pub struct DesktopIndex {
    entries: HashMap<String, DesktopEntry>,
}

enum Match<'a> {
    None,
    One(&'a str),
    Many,
}

impl DesktopIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, id: &str, e: DesktopEntry) {
        self.entries
            .insert(id.strip_suffix(".desktop").unwrap_or(id).to_owned(), e);
    }

    pub fn get(&self, id: &str) -> Option<&DesktopEntry> {
        self.entries.get(id.strip_suffix(".desktop").unwrap_or(id))
    }

    /// Match by executable basename, then by `StartupWMClass`. Ambiguity (more
    /// than one candidate) gives no answer rather than a guess.
    pub fn find_by_exe(&self, exe: &str) -> Option<&str> {
        match self.matches(exe, exec_base) {
            Match::None => match self.matches(exe, wm_class) {
                Match::One(id) => Some(id),
                _ => None,
            },
            Match::One(id) => Some(id),
            Match::Many => None,
        }
    }

    fn matches(&self, exe: &str, sel: fn(&DesktopEntry) -> Option<&str>) -> Match<'_> {
        let mut hit = self
            .entries
            .iter()
            .filter(|(_, e)| sel(e).is_some_and(|v| v == exe));
        match (hit.next(), hit.next()) {
            (None, _) => Match::None,
            (Some((id, _)), None) => Match::One(id.as_str()),
            _ => Match::Many,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn exec_base(e: &DesktopEntry) -> Option<&str> {
    e.exec_base.as_deref()
}

fn wm_class(e: &DesktopEntry) -> Option<&str> {
    e.startup_wm_class.as_deref()
}

/// Parse the `[Desktop Entry]` group of a `.desktop` file. `NoDisplay`/`Hidden`
/// entries still index (a hidden helper is still an app identity).
pub fn parse_desktop(text: &str) -> DesktopEntry {
    let mut e = DesktopEntry::default();
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "Name" => e.name = Some(v.trim().to_owned()),
            "Icon" => e.icon = Some(v.trim().to_owned()),
            "StartupWMClass" => e.startup_wm_class = Some(v.trim().to_owned()),
            "Terminal" => e.terminal = v.trim() == "true",
            "Exec" => {
                e.exec = Some(v.trim().to_owned());
                e.exec_base = v
                    .split_whitespace()
                    .next()
                    .map(|w| w.rsplit('/').next().unwrap_or(w).to_owned())
            }
            _ => {}
        }
    }
    e
}

/// `[Application] name=` from `/.flatpak-info`.
pub fn parse_flatpak_info(text: &str) -> Option<String> {
    let mut in_app = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_app = line == "[Application]";
        } else if in_app {
            if let Some(v) = line.strip_prefix("name=") {
                return valid_app_id(v.trim()).then(|| v.trim().to_owned());
            }
        }
    }
    None
}

/// App id out of a systemd unit name following the XDG convention
/// `app[-<launcher>]-<id>[-<rand>|@<rand>].(scope|service)`, `-` in the id
/// escaped as `\x2d`.
pub fn app_id_from_unit(unit: &str) -> Option<String> {
    let stem = unit
        .strip_suffix(".scope")
        .or_else(|| unit.strip_suffix(".service"))?;
    let stem = stem.strip_prefix("app-")?;
    let stem = stem.split('@').next().unwrap_or(stem);
    let parts: Vec<&str> = stem.split('-').collect();
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let id = match parts.as_slice() {
        [id] => id,
        [a, b] if all_digits(b) => a,
        [_launcher, id] => id,
        [_launcher, id, rand] if all_digits(rand) => id,
        _ => return None,
    };
    let id = id.replace("\\x2d", "-");
    valid_app_id(&id).then_some(id)
}

/// The innermost `app-*` unit in a cgroup file (`0::/a/b/app-x.scope`).
pub fn app_id_from_cgroup(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("0::"))
        .chain(text.lines().filter_map(|l| l.rsplit(':').next()))
        .find_map(|path| {
            path.rsplit('/')
                .find(|c| c.starts_with("app-") && app_id_from_unit(c).is_some())
                .and_then(app_id_from_unit)
        })
}

/// Cheap sanity check; ids are rendered to the user and used as file names.
pub fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn requester(id: &str, source: IdentitySource, index: &DesktopIndex) -> Requester {
    let e = index.get(id);
    Requester {
        app_id: id.to_owned(),
        display_name: e.and_then(|e| e.name.clone()),
        icon: e.and_then(|e| e.icon.clone()),
        source,
    }
}

/// The identity to show for a caller. Order: Flatpak, security-context tag,
/// systemd scope/unit, `.desktop` match by executable, else `host`.
pub fn resolve(facts: &Facts<'_>, index: &DesktopIndex) -> Requester {
    if let Some(id) = facts.flatpak_info.and_then(parse_flatpak_info) {
        return requester(&id, IdentitySource::Flatpak, index);
    }
    if let Some(t) = facts.security_context {
        if valid_app_id(&t.app_id) {
            return requester(&t.app_id, IdentitySource::SecurityContext, index);
        }
    }
    let from_unit = facts
        .unit
        .and_then(app_id_from_unit)
        .or_else(|| facts.cgroup.and_then(app_id_from_cgroup));
    if let Some(id) = from_unit {
        return requester(&id, IdentitySource::DesktopFile, index);
    }
    if let Some(id) = facts.exe_name.and_then(|x| index.find_by_exe(x)) {
        return requester(id, IdentitySource::DesktopFile, index);
    }
    host()
}

pub fn host() -> Requester {
    Requester {
        app_id: "host".into(),
        display_name: None,
        icon: None,
        source: IdentitySource::Host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> DesktopIndex {
        let mut i = DesktopIndex::new();
        i.insert(
            "org.mozilla.firefox.desktop",
            parse_desktop(
                "[Desktop Entry]\nName=Firefox\nIcon=firefox\nExec=/usr/bin/firefox %u\n[Desktop Action x]\nName=No\n",
            ),
        );
        i.insert(
            "foot",
            parse_desktop("[Desktop Entry]\nName=Foot\nExec=foot\nStartupWMClass=foot\n"),
        );
        i
    }

    #[test]
    fn desktop_parse() {
        let e = index().get("org.mozilla.firefox").cloned().unwrap();
        assert_eq!(e.name.as_deref(), Some("Firefox"));
        assert_eq!(e.exec_base.as_deref(), Some("firefox"));
    }

    #[test]
    fn unit_names() {
        assert_eq!(
            app_id_from_unit("app-org.mozilla.firefox-1234.scope").as_deref(),
            Some("org.mozilla.firefox")
        );
        assert_eq!(
            app_id_from_unit("app-gnome-org.gnome.Nautilus-3.scope").as_deref(),
            Some("org.gnome.Nautilus")
        );
        assert_eq!(
            app_id_from_unit("app-foot@abc123.service").as_deref(),
            Some("foot")
        );
        assert_eq!(
            app_id_from_unit("app-my\\x2dapp.service").as_deref(),
            Some("my-app")
        );
        assert_eq!(app_id_from_unit("dbus.service"), None);
        assert_eq!(app_id_from_unit("app-a-b-c-d.scope"), None);
    }

    #[test]
    fn cgroup_innermost() {
        let t = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-foot@x.service\n";
        assert_eq!(app_id_from_cgroup(t).as_deref(), Some("foot"));
        assert_eq!(app_id_from_cgroup("0::/init.scope\n"), None);
    }

    #[test]
    fn flatpak_wins() {
        let facts = Facts {
            flatpak_info: Some("[Application]\nname=org.x.App\n"),
            unit: Some("app-foot.service"),
            ..Facts::default()
        };
        let r = resolve(&facts, &index());
        assert_eq!(r.app_id, "org.x.App");
        assert_eq!(r.source, IdentitySource::Flatpak);
    }

    #[test]
    fn security_context_then_unit_then_exe() {
        let tag = SecurityContextTag {
            app_id: "org.y.App".into(),
            engine: Some("flatpak".into()),
        };
        let r = resolve(
            &Facts {
                security_context: Some(&tag),
                ..Facts::default()
            },
            &index(),
        );
        assert_eq!(r.source, IdentitySource::SecurityContext);

        let r = resolve(
            &Facts {
                unit: Some("app-org.mozilla.firefox-9.scope"),
                ..Facts::default()
            },
            &index(),
        );
        assert_eq!(r.display_name.as_deref(), Some("Firefox"));
        assert_eq!(r.source, IdentitySource::DesktopFile);

        let r = resolve(
            &Facts {
                exe_name: Some("firefox"),
                ..Facts::default()
            },
            &index(),
        );
        assert_eq!(r.app_id, "org.mozilla.firefox");
    }

    #[test]
    fn unknown_is_host() {
        let r = resolve(&Facts::default(), &index());
        assert_eq!(r.app_id, "host");
        assert_eq!(r.source, IdentitySource::Host);
        let r = resolve(
            &Facts {
                exe_name: Some("nope"),
                ..Facts::default()
            },
            &index(),
        );
        assert_eq!(r.source, IdentitySource::Host);
    }

    #[test]
    fn hostile_ids_rejected() {
        let tag = SecurityContextTag {
            app_id: "../../etc".into(),
            engine: None,
        };
        let r = resolve(
            &Facts {
                security_context: Some(&tag),
                flatpak_info: Some("[Application]\nname=a/b\n"),
                ..Facts::default()
            },
            &index(),
        );
        assert_eq!(r.source, IdentitySource::Host);
        assert!(!valid_app_id(""));
        assert!(!valid_app_id(".."));
    }

    #[test]
    fn ambiguous_exe_gives_nothing() {
        let mut i = index();
        i.insert("foot2", parse_desktop("[Desktop Entry]\nExec=foot\n"));
        assert_eq!(i.find_by_exe("foot"), None);
    }
}
