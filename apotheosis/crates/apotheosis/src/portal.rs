// SPDX-License-Identifier: AGPL-3.0-only
//! The settings portal's reduce-motion preference (T-08 §2, T-20 §6): `interface.motion
//! system` is `reduced` while `org.freedesktop.appearance` `reduced-motion` is 1, else
//! `full`. A thread of its own reads it once and then follows `SettingChanged`; it reports
//! each value to the main loop. No session bus, no portal, or no such key (an older
//! portal) reports nothing, which leaves `full`. Logs carry no values beyond the flag.

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.Settings";
pub(crate) const NAMESPACE: &str = "org.freedesktop.appearance";
pub(crate) const KEY: &str = "reduced-motion";

/// `reduced-motion` as a flag: `1` reduce, `0` no preference. Accepts the value wrapped
/// in a variant (the deprecated `Read` adds one level); anything else is no answer.
pub(crate) fn reduced(v: &Value<'_>) -> Option<bool> {
    match v {
        Value::U32(n) => Some(*n == 1),
        Value::Value(inner) => reduced(inner),
        _ => None,
    }
}

/// The current value, by `ReadOne`, else the older `Read`.
fn read(conn: &Connection) -> Option<bool> {
    for method in ["ReadOne", "Read"] {
        let Ok(reply) = conn.call_method(Some(DEST), PATH, Some(IFACE), method, &(NAMESPACE, KEY))
        else {
            continue;
        };
        if let Ok(v) = reply.body().deserialize::<OwnedValue>() {
            return reduced(&v);
        }
    }
    None
}

/// Report the value now and on every change until the bus goes away. Subscribes before
/// reading, so a change between the two is not lost.
pub(crate) fn watch(conn: &Connection, on: &mut impl FnMut(bool)) {
    let signals = Proxy::new(conn, DEST, PATH, IFACE)
        .and_then(|p| p.receive_signal_with_args("SettingChanged", &[(0, NAMESPACE), (1, KEY)]));
    let first = read(conn);
    if let Some(r) = first {
        on(r);
    }
    let Ok(signals) = signals else {
        tracing::debug!(found = first.is_some(), "settings portal: no change signal");
        return;
    };
    for msg in signals {
        let Ok((ns, key, v)) = msg.body().deserialize::<(String, String, OwnedValue)>() else {
            continue;
        };
        if ns == NAMESPACE
            && key == KEY
            && let Some(r) = reduced(&v)
        {
            on(r);
        }
    }
}

/// Follow the session bus's portal on a thread of its own. `on` runs on that thread.
pub(crate) fn spawn(mut on: impl FnMut(bool) + Send + 'static) {
    let r = std::thread::Builder::new()
        .name("apoth-portal".into())
        .spawn(move || match Connection::session() {
            Ok(conn) => watch(&conn, &mut on),
            Err(_) => tracing::debug!("no session bus: interface.motion system is full"),
        });
    if r.is_err() {
        tracing::warn!("could not start the settings-portal thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn values_decode() {
        assert_eq!(reduced(&Value::U32(1)), Some(true));
        assert_eq!(reduced(&Value::U32(0)), Some(false));
        // Unknown future values are "no preference".
        assert_eq!(reduced(&Value::U32(7)), Some(false));
        assert_eq!(reduced(&Value::Value(Box::new(Value::U32(1)))), Some(true));
        assert_eq!(reduced(&Value::Bool(true)), None);
        assert_eq!(reduced(&Value::from("1")), None);
    }

    /// A fake portal on a private bus: the value is read at start and follows the signal.
    struct FakeSettings(u32);

    #[zbus::interface(name = "org.freedesktop.portal.Settings")]
    impl FakeSettings {
        fn read_one(&self, ns: &str, key: &str) -> zbus::fdo::Result<OwnedValue> {
            if ns == NAMESPACE && key == KEY {
                Ok(OwnedValue::from(self.0))
            } else {
                Err(zbus::fdo::Error::Failed("no such key".into()))
            }
        }
    }

    /// A private `dbus-daemon`, killed by its own pid on drop.
    struct Bus(std::process::Child, String);

    impl Bus {
        fn start() -> Option<Bus> {
            use std::io::BufRead;
            let mut child = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let mut line = String::new();
            let out = child.stdout.take()?;
            std::io::BufReader::new(out).read_line(&mut line).ok()?;
            Some(Bus(child, line.trim().to_owned()))
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn follows_a_portal_on_the_bus() {
        let Some(bus) = Bus::start() else {
            assert!(
                std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
                "dbus-daemon is required"
            );
            return;
        };
        let server = zbus::blocking::connection::Builder::address(bus.1.as_str())
            .and_then(|b| b.name(DEST))
            .and_then(|b| b.serve_at(PATH, FakeSettings(1)))
            .and_then(|b| b.build())
            .expect("fake portal");
        let client = zbus::blocking::connection::Builder::address(bus.1.as_str())
            .unwrap()
            .build()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let t = std::thread::spawn(move || watch(&client, &mut |r| tx.send(r).unwrap()));
        let wait = Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(wait), Ok(true), "initial ReadOne");
        // Another key's change is not ours.
        server
            .emit_signal(
                None::<&str>,
                PATH,
                IFACE,
                "SettingChanged",
                &(NAMESPACE, "color-scheme", Value::U32(0)),
            )
            .unwrap();
        server
            .emit_signal(
                None::<&str>,
                PATH,
                IFACE,
                "SettingChanged",
                &(NAMESPACE, KEY, Value::U32(0)),
            )
            .unwrap();
        assert_eq!(rx.recv_timeout(wait), Ok(false), "live change");
        server
            .emit_signal(
                None::<&str>,
                PATH,
                IFACE,
                "SettingChanged",
                &(NAMESPACE, KEY, Value::U32(1)),
            )
            .unwrap();
        assert_eq!(rx.recv_timeout(wait), Ok(true));
        drop(server);
        drop(bus);
        // The bus going away ends the watch.
        let _ = t.join();
    }

    #[test]
    fn no_portal_reports_nothing() {
        let Some(bus) = Bus::start() else {
            return;
        };
        let client = zbus::blocking::connection::Builder::address(bus.1.as_str())
            .unwrap()
            .build()
            .unwrap();
        // No service owns the name: no value. The watch then idles on the signal stream
        // (a portal that starts later is still followed) until the bus goes.
        let (tx, rx) = mpsc::channel();
        let t = std::thread::spawn(move || watch(&client, &mut |r| tx.send(r).unwrap()));
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
        drop(bus);
        let _ = t.join();
    }
}
