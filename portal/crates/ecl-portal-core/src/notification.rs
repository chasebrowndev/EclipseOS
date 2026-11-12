// SPDX-License-Identifier: AGPL-3.0-only

//! Map a portal `AddNotification` dict onto an `org.freedesktop.Notifications`
//! `Notify` call. Content is the app's, shown verbatim; nothing is logged.

use crate::options::Options;
use crate::val::Val;

#[derive(Debug, Clone, PartialEq)]
pub struct NotifyCall {
    pub app_name: String,
    pub icon: String,
    pub summary: String,
    pub body: String,
    /// Flat `[key, label, key, label, ...]` as `Notify` wants.
    pub actions: Vec<String>,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
    /// -1 server default, 0 never expire.
    pub expire_timeout: i32,
}

/// `priority`: low, normal, high, urgent.
pub fn urgency(priority: Option<&str>) -> u8 {
    match priority {
        Some("low") => 0,
        Some("high") | Some("urgent") => 2,
        _ => 1,
    }
}

/// Icon: `(sv)` where the first is `themed` (`as` of names) or `file`/`bytes`.
/// Only the first themed name is used; file URIs map to their path.
pub fn icon_name(icon: Option<&Val>) -> String {
    let Some([kind, data]) = icon.and_then(Val::as_struct) else {
        return String::new();
    };
    match (kind.as_str(), data) {
        (Some("themed"), d) => d
            .as_strs()
            .and_then(|v| v.into_iter().next())
            .unwrap_or_default(),
        (Some("file"), d) => d
            .as_str()
            .and_then(crate::filechooser::uri_to_path)
            .and_then(|p| String::from_utf8(p).ok())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Buttons `aa{sv}`: each with `label`, `action`. The default action becomes
/// the `default` key that Notifications servers show as a click.
pub fn to_notify(app_name: &str, o: &Options) -> NotifyCall {
    let mut actions = Vec::new();
    if let Some(a) = o.str("default-action") {
        actions.push(a.to_owned());
        actions.push(String::new());
    }
    if let Some(list) = o.get("buttons").and_then(Val::as_list) {
        for b in list {
            let Val::Dict(d) = b else { continue };
            let get = |k: &str| d.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.as_str());
            if let (Some(label), Some(action)) = (get("label"), get("action")) {
                actions.push(action.to_owned());
                actions.push(label.to_owned());
            }
        }
    }
    NotifyCall {
        app_name: app_name.to_owned(),
        icon: icon_name(o.get("icon")),
        summary: o.str("title").unwrap_or_default().to_owned(),
        body: o.str("body").unwrap_or_default().to_owned(),
        actions,
        urgency: urgency(o.str("priority")),
        expire_timeout: -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_fields() {
        let o = Options::from_pairs([
            ("title".into(), Val::Str("Hi".into())),
            ("body".into(), Val::Str("There".into())),
            ("priority".into(), Val::Str("urgent".into())),
            ("default-action".into(), Val::Str("open".into())),
            (
                "icon".into(),
                Val::Struct(vec![
                    Val::Str("themed".into()),
                    Val::Strs(vec!["mail".into(), "x".into()]),
                ]),
            ),
            (
                "buttons".into(),
                Val::List(vec![
                    Val::Dict(vec![
                        ("label".into(), Val::Str("Reply".into())),
                        ("action".into(), Val::Str("reply".into())),
                    ]),
                    Val::Dict(vec![("label".into(), Val::Str("No action".into()))]),
                ]),
            ),
        ]);
        let n = to_notify("org.x.App", &o);
        assert_eq!(n.summary, "Hi");
        assert_eq!(n.urgency, 2);
        assert_eq!(n.icon, "mail");
        assert_eq!(n.actions, vec!["open", "", "reply", "Reply"]);
    }

    #[test]
    fn empty_is_safe() {
        let n = to_notify("a", &Options::new());
        assert_eq!(n.summary, "");
        assert_eq!(n.urgency, 1);
        assert!(n.actions.is_empty());
        assert_eq!(icon_name(Some(&Val::Bool(true))), "");
    }

    #[test]
    fn file_icon() {
        let v = Val::Struct(vec![
            Val::Str("file".into()),
            Val::Str("file:///a%20b.png".into()),
        ]);
        assert_eq!(icon_name(Some(&v)), "/a b.png");
    }
}
