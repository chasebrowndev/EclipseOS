// SPDX-License-Identifier: AGPL-3.0-only
//! `org.freedesktop.impl.portal.Settings`: the xdg-desktop-portal backend that
//! tells GTK, Qt, Firefox and friends whether the session is dark or light
//! (ADR 0084).
//!
//! Only the `org.freedesktop.appearance` namespace is served: `color-scheme`,
//! `accent-color` and `contrast`. The source of truth is the compositor's
//! `get_theme` reply at start and its `theme` event afterwards, over the
//! control socket. The bus side never waits for the compositor: until the
//! first reply, and whenever the socket is down, the last known (initially
//! dark) values are served.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value as Json;
use zbus::blocking::connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::zvariant::{OwnedValue, Structure, Value};
use zbus::{interface, DBusError};

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.abyss";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.impl.portal.Settings";
pub const NAMESPACE: &str = "org.freedesktop.appearance";

const RETRY: Duration = Duration::from_secs(3);

/// The appearance settings this backend publishes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Appearance {
    /// 1 prefer dark, 2 prefer light (0 "no preference" is never published).
    pub color_scheme: u32,
    /// sRGB, each channel 0..=1.
    pub accent: (f64, f64, f64),
    /// 0 = no preference.
    pub contrast: u32,
}

impl Default for Appearance {
    /// Dark, with the gold accent, until the compositor says otherwise.
    fn default() -> Self {
        Appearance {
            color_scheme: 1,
            accent: (0.847, 0.686, 0.282),
            contrast: 0,
        }
    }
}

impl Appearance {
    /// Map a `get_theme` reply / `theme` event payload. `None` if it lacks a
    /// readable scheme or accent: a half-understood payload changes nothing.
    pub fn from_theme(theme: &Json) -> Option<Appearance> {
        let color_scheme = match theme.get("scheme")?.as_str()? {
            "dark" => 1,
            "light" => 2,
            _ => return None,
        };
        let accent = parse_hex_rgb(theme.get("seeds")?.get("accent")?.as_str()?)?;
        Some(Appearance {
            color_scheme,
            accent,
            contrast: 0,
        })
    }

    /// The `(key, value)` pairs of the appearance namespace.
    pub fn entries(&self) -> [(&'static str, Value<'static>); 3] {
        let (r, g, b) = self.accent;
        [
            ("color-scheme", Value::U32(self.color_scheme)),
            ("accent-color", Value::Structure(Structure::from((r, g, b)))),
            ("contrast", Value::U32(self.contrast)),
        ]
    }

    /// One key's value, `None` for an unknown key.
    pub fn get(&self, key: &str) -> Option<Value<'static>> {
        self.entries()
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    /// Keys whose value differs from `old`.
    pub fn changed_keys(&self, old: &Appearance) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.color_scheme != old.color_scheme {
            keys.push("color-scheme");
        }
        if self.accent != old.accent {
            keys.push("accent-color");
        }
        if self.contrast != old.contrast {
            keys.push("contrast");
        }
        keys
    }
}

/// `#rrggbb` to sRGB doubles in 0..=1.
fn parse_hex_rgb(s: &str) -> Option<(f64, f64, f64)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let ch = |i: usize| {
        u8::from_str_radix(&h[i..i + 2], 16)
            .ok()
            .map(|v| f64::from(v) / 255.0)
    };
    Some((ch(0)?, ch(2)?, ch(4)?))
}

/// Does a `ReadAll` namespace filter match `ns`? Empty matches everything; a
/// trailing `*` is a prefix match (the portal spec's glob).
fn ns_matches(pattern: &str, ns: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    match pattern.strip_suffix('*') {
        Some(prefix) => ns.starts_with(prefix.trim_end_matches('.')),
        None => pattern == ns,
    }
}

#[derive(Debug, DBusError)]
#[zbus(prefix = "org.freedesktop.portal.Error")]
enum PortalError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NotFound(String),
}

struct Settings {
    state: Arc<Mutex<Appearance>>,
}

impl Settings {
    fn current(&self) -> Appearance {
        *self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn owned(v: Value<'static>) -> OwnedValue {
    // Only u32 and a struct of f64: no file descriptors, so this cannot fail.
    OwnedValue::try_from(v).unwrap_or_else(|_| OwnedValue::from(0u32))
}

#[interface(name = "org.freedesktop.impl.portal.Settings")]
impl Settings {
    fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> std::collections::HashMap<String, std::collections::HashMap<String, OwnedValue>> {
        let mut out = std::collections::HashMap::new();
        if namespaces.is_empty() || namespaces.iter().any(|p| ns_matches(p, NAMESPACE)) {
            let map = self
                .current()
                .entries()
                .into_iter()
                .map(|(k, v)| (k.to_owned(), owned(v)))
                .collect();
            out.insert(NAMESPACE.to_owned(), map);
        }
        out
    }

    /// Deprecated in the portal spec but still called by older clients.
    fn read(&self, namespace: String, key: String) -> Result<OwnedValue, PortalError> {
        self.read_one(namespace, key)
    }

    fn read_one(&self, namespace: String, key: String) -> Result<OwnedValue, PortalError> {
        if namespace == NAMESPACE {
            if let Some(v) = self.current().get(&key) {
                return Ok(owned(v));
            }
        }
        Err(PortalError::NotFound(format!("{namespace} {key}")))
    }

    #[zbus(property(emits_changed_signal = "const"), name = "version")]
    fn version(&self) -> u32 {
        2
    }

    #[zbus(signal)]
    async fn setting_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        namespace: &str,
        key: &str,
        value: Value<'_>,
    ) -> zbus::Result<()>;
}

/// The running service. Dropping it leaves the bus.
pub struct Portal {
    _connection: zbus::blocking::Connection,
}

/// Store `new` and announce the keys that changed. Held under the lock so
/// signals leave in the order the values changed.
fn apply(state: &Mutex<Appearance>, conn: &zbus::blocking::Connection, new: Appearance) {
    let mut cur = state.lock().unwrap_or_else(|p| p.into_inner());
    let changed = new.changed_keys(&cur);
    *cur = new;
    for key in changed {
        if let Some(value) = new.get(key) {
            let _ = conn.emit_signal(
                None::<&str>,
                OBJECT_PATH,
                IFACE,
                "SettingChanged",
                &(NAMESPACE, key, value),
            );
        }
    }
}

/// Own the backend name and follow the compositor's theme.
///
/// Fails if the name is taken -- we never steal it.
pub fn spawn() -> zbus::Result<Portal> {
    let state = Arc::new(Mutex::new(Appearance::default()));
    let connection = connection::Builder::session()?
        .serve_at(
            OBJECT_PATH,
            Settings {
                state: Arc::clone(&state),
            },
        )?
        .build()?;
    match connection.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())? {
        RequestNameReply::PrimaryOwner => {}
        _ => return Err(zbus::Error::NameTaken),
    }
    let conn = connection.clone();
    thread::Builder::new()
        .name("eclipse-portal-ipc".into())
        .spawn(move || bridge(&state, &conn))
        .map_err(|e| zbus::Error::InputOutput(Arc::new(e)))?;
    Ok(Portal {
        _connection: connection,
    })
}

/// Follow the compositor forever: connect, read the theme, then apply each
/// `theme` event. A lost socket (compositor restart) reconnects and re-reads.
fn bridge(state: &Mutex<Appearance>, conn: &zbus::blocking::Connection) {
    loop {
        let _ = follow(state, conn);
        thread::sleep(RETRY);
    }
}

fn follow(state: &Mutex<Appearance>, conn: &zbus::blocking::Connection) -> Result<(), ec_ipc::Error> {
    let mut client = ec_ipc::Client::connect()?;
    // Subscribe first so a change between the read and the subscription is
    // not lost; the read then covers anything before it.
    client.subscribe(&[ec_ipc::EventKind::Theme])?;
    let theme = client.call("get_theme", Json::Null)?;
    if let Some(a) = Appearance::from_theme(&theme) {
        apply(state, conn, a);
    }
    loop {
        let ev = client.wait_event()?;
        if ev.kind == ec_ipc::EventKind::Theme {
            if let Some(a) = Appearance::from_theme(&ev.data) {
                apply(state, conn, a);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn theme(scheme: &str, accent: &str) -> Json {
        json!({"scheme": scheme, "setting": "auto", "theme": scheme,
               "seeds": {"base": "#000000", "ink": "#ffffff", "accent": accent,
                         "accent_text": null, "frost_tint": "#00000080", "rim": 0.5}})
    }

    #[test]
    fn scheme_maps_to_portal_values() {
        let dark = Appearance::from_theme(&theme("dark", "#ff0000")).unwrap();
        let light = Appearance::from_theme(&theme("light", "#ff0000")).unwrap();
        assert_eq!(dark.color_scheme, 1);
        assert_eq!(light.color_scheme, 2);
        assert_eq!(dark.contrast, 0);
    }

    #[test]
    fn accent_is_srgb_zero_to_one() {
        let a = Appearance::from_theme(&theme("dark", "#ff8000")).unwrap();
        assert_eq!(a.accent.0, 1.0);
        assert!((a.accent.1 - 128.0 / 255.0).abs() < 1e-12);
        assert_eq!(a.accent.2, 0.0);
    }

    #[test]
    fn bad_payloads_are_ignored() {
        assert!(Appearance::from_theme(&json!({})).is_none());
        assert!(Appearance::from_theme(&theme("sepia", "#ff0000")).is_none());
        assert!(Appearance::from_theme(&theme("dark", "red")).is_none());
        assert!(Appearance::from_theme(&theme("dark", "#ff00")).is_none());
        assert!(Appearance::from_theme(&theme("dark", "#ééé")).is_none());
    }

    #[test]
    fn only_changed_keys_are_reported() {
        let a = Appearance::from_theme(&theme("dark", "#ff0000")).unwrap();
        assert!(a.changed_keys(&a).is_empty());
        let b = Appearance::from_theme(&theme("light", "#ff0000")).unwrap();
        assert_eq!(b.changed_keys(&a), ["color-scheme"]);
        let c = Appearance::from_theme(&theme("light", "#00ff00")).unwrap();
        assert_eq!(c.changed_keys(&b), ["accent-color"]);
        assert_eq!(c.changed_keys(&a), ["color-scheme", "accent-color"]);
    }

    #[test]
    fn keys_are_looked_up_in_the_appearance_namespace_only() {
        let a = Appearance::default();
        assert_eq!(a.get("color-scheme"), Some(Value::U32(1)));
        assert_eq!(a.get("contrast"), Some(Value::U32(0)));
        assert!(a.get("accent-color").is_some());
        assert!(a.get("nope").is_none());
    }

    #[test]
    fn namespace_filters_follow_the_portal_glob() {
        assert!(ns_matches("", NAMESPACE));
        assert!(ns_matches(NAMESPACE, NAMESPACE));
        assert!(ns_matches("org.freedesktop.*", NAMESPACE));
        assert!(ns_matches("org.freedesktop.appearance*", NAMESPACE));
        assert!(!ns_matches("org.gnome.*", NAMESPACE));
        assert!(!ns_matches("org.freedesktop.appearanc", NAMESPACE));
    }

    #[test]
    fn default_is_dark() {
        assert_eq!(Appearance::default().color_scheme, 1);
    }
}
