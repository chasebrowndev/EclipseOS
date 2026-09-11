// SPDX-License-Identifier: AGPL-3.0-only
//! The server against a real bus.
//!
//! `#[ignore]`d on purpose: it needs `dbus-daemon` on PATH, and it must never
//! run against the human's session bus — a second daemon taking
//! `org.freedesktop.Notifications` would break the desktop it is running on.
//! It spawns a private bus and points `DBUS_SESSION_BUS_ADDRESS` at that.
//!
//! Run it by hand:
//!
//! ```text
//! cargo test -p eclipse-services --test live_bus -- --ignored
//! ```

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use eclipse_services::notifications::{self, CloseReason, Event, Urgency};

/// A `dbus-daemon` of our own, killed when the guard drops.
struct PrivateBus(Child);

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn private_bus() -> (PrivateBus, String) {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("dbus-daemon on PATH");
    let mut address = String::new();
    BufReader::new(child.stdout.take().expect("piped stdout"))
        .read_line(&mut address)
        .expect("the daemon prints its address first");
    (PrivateBus(child), address.trim().to_owned())
}

fn next_event(handle: &notifications::Notifications) -> Event {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = handle.try_recv() {
            return event;
        }
        assert!(Instant::now() < deadline, "no event within five seconds");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "needs dbus-daemon; spawns a private bus"]
fn a_notify_call_arrives_as_an_event() {
    let (_bus, address) = private_bus();
    // SAFETY: single-threaded, before any bus connection is made.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };

    let handle = notifications::spawn().expect("the private bus has no other daemon on it");

    let sent = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications.Notify",
            "string:test-client",
            "uint32:0",
            "string:dialog-information",
            "string:A summary",
            "string:A body",
            "array:string:ok,OK",
            "dict:string:variant:urgency,byte:2",
            "int32:0",
        ])
        .output()
        .expect("dbus-send runs");
    assert!(sent.status.success(), "{}", String::from_utf8_lossy(&sent.stderr));

    let Event::Posted(notification) = next_event(&handle) else {
        panic!("expected a posted notification");
    };
    assert_eq!(notification.id, 1, "ids start at one, not zero");
    assert_eq!(notification.app_name, "test-client");
    assert_eq!(notification.summary, "A summary");
    assert_eq!(notification.urgency, Urgency::Critical);
    assert_eq!(notification.actions.len(), 1);
    assert_eq!(notification.actions[0].label, "OK");
    // Critical and asked for zero: it stays until the human acts.
    assert_eq!(notification.expires_in, None);

    // And the way back out: the human's click reaches the bus.
    handle.invoke(notification.id, "ok");
    handle.close(notification.id, CloseReason::Dismissed);
}

#[test]
#[ignore = "needs dbus-daemon; spawns a private bus"]
fn the_name_is_never_stolen_from_a_running_daemon() {
    let (_bus, address) = private_bus();
    // SAFETY: single-threaded, before any bus connection is made.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };

    let _first = notifications::spawn().expect("the first one takes the name");
    assert!(
        notifications::spawn().is_err(),
        "a second server must fail rather than take the name from the first"
    );
}
