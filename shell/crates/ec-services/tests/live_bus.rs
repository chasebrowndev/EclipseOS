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
//! cargo test -p ec-services --test live_bus -- --ignored
//! ```

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ec_services::notifications::{self, CloseReason, Event, Urgency};

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

/// Point the history file and rules at a fresh directory: the server persists
/// to `$XDG_STATE_HOME`, and a test must never touch the human's.
fn isolate_state() {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ec-live-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    // SAFETY: tests that call this run before any server thread reads them.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", dir.join("state"));
        std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
    }
}

fn notify_via_dbus_send(summary: &str, extra: &str) {
    let out = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications.Notify",
            "string:app",
            "uint32:0",
            "string:",
            &format!("string:{summary}"),
            "string:body",
            "array:string:",
            extra,
            "int32:0",
        ])
        .output()
        .expect("dbus-send runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
#[ignore = "needs dbus-daemon; spawns a private bus"]
fn modes_unread_and_mark_read_over_the_bus() {
    use ec_services::notifications::{HistoryClient, Mode, PageFilter};

    let (_bus, address) = private_bus();
    // SAFETY: single-threaded, before any bus connection is made.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
    isolate_state();
    let handle = notifications::spawn().expect("name is free");
    let mut client = HistoryClient::at(&address);
    let unread = client.unread_changes();

    assert_eq!(client.mode(), Mode::Normal);
    notify_via_dbus_send("shown", "dict:string:variant:x,byte:1");
    assert!(matches!(next_event(&handle), Event::Posted(_)));
    assert_eq!(unread.recv_timeout(Duration::from_secs(5)), Ok(1));

    assert!(client.set_mode(Mode::Quiet));
    assert_eq!(client.mode(), Mode::Quiet);
    notify_via_dbus_send("quiet", "dict:string:variant:x,byte:1");
    assert_eq!(unread.recv_timeout(Duration::from_secs(5)), Ok(2));
    // No popup for the quiet one (only the withdraw-if-replacing close).
    assert!(!matches!(next_event(&handle), Event::Posted(_)));

    assert!(client.set_mode(Mode::Dnd));
    notify_via_dbus_send("dropped", "dict:string:variant:x,byte:1");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(client.unread_count(), 2, "dnd stores nothing");

    let page = client.page(PageFilter::Unread, 0, 10);
    assert_eq!(page.entries.len(), 2);
    assert!(
        page.entries.iter().all(|e| e.entry.live),
        "quiet entries are invokable too"
    );
    client.mark_all_read();
    assert_eq!(client.unread_count(), 0);
    assert_eq!(client.page(PageFilter::Read, 0, 10).total, 2);
}

#[test]
#[ignore = "needs dbus-daemon; spawns a private bus"]
fn viewing_suppresses_popups_but_not_critical_and_ends_with_the_client() {
    use ec_services::notifications::HistoryClient;

    let (_bus, address) = private_bus();
    // SAFETY: single-threaded, before any bus connection is made.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
    isolate_state();
    let handle = notifications::spawn().expect("name is free");
    let mut client = HistoryClient::at(&address);

    assert!(client.set_viewing(true));
    notify_via_dbus_send("hidden", "dict:string:variant:x,byte:1");
    assert_eq!(client.unread_count(), 1);
    assert!(!matches!(next_event(&handle), Event::Posted(_)));

    notify_via_dbus_send("urgent", "dict:string:variant:urgency,byte:2");
    loop {
        match next_event(&handle) {
            Event::Posted(n) => {
                assert_eq!(n.summary, "urgent");
                break;
            }
            _ => continue,
        }
    }

    // The setting client leaves the bus without clearing: popups return.
    drop(client);
    std::thread::sleep(Duration::from_millis(500));
    notify_via_dbus_send("again", "dict:string:variant:x,byte:1");
    loop {
        if let Event::Posted(n) = next_event(&handle) {
            assert_eq!(n.summary, "again");
            break;
        }
    }
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
    isolate_state();

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
fn history_lists_removes_and_refuses_a_stale_invoke() {
    use ec_services::notifications::HistoryClient;

    let (_bus, address) = private_bus();
    // SAFETY: single-threaded, before any bus connection is made.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
    isolate_state();
    let handle = notifications::spawn().expect("name is free");

    let mut client = HistoryClient::at(&address);
    let changes = client.changes();
    let notify = |summary: &str, extra: &str| {
        let out = Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications.Notify",
                "string:app",
                "uint32:0",
                "string:icon",
                &format!("string:{summary}"),
                "string:body",
                "array:string:ok,OK",
                extra,
                "int32:0",
            ])
            .output()
            .expect("dbus-send runs");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    notify("one", "dict:string:variant:urgency,byte:1");
    notify("two", "dict:string:variant:urgency,byte:1");
    notify("quiet", "dict:string:variant:transient,boolean:true");

    // The history is not reachable through the notification name.
    let out = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.Notifications",
            "/org/eclipse/NotificationHistory",
            "org.eclipse.NotificationHistory.List",
        ])
        .output()
        .expect("dbus-send runs");
    assert!(
        !out.status.success(),
        "history must not answer on the notification name"
    );

    let list = client.list();
    let summaries: Vec<&str> = list.iter().map(|e| e.summary.as_str()).collect();
    assert_eq!(summaries, ["two", "one"], "newest first, transient absent");
    assert!(list.iter().all(|e| e.live));

    // The human dismissed "one": still in history, no longer invokable.
    handle.close(list[1].id, CloseReason::Dismissed);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!client.invoke_action(list[1].id, "ok"));
    assert!(client.list().iter().any(|e| e.id == list[1].id && !e.live));

    // "two" is live: invoking works once and closes it.
    assert!(client.invoke_action(list[0].id, "ok"));
    assert!(!client.invoke_action(list[0].id, "ok"));

    client.remove(list[0].id);
    assert_eq!(client.list().len(), 1);
    client.clear();
    assert!(client.list().is_empty());

    // Two stored Notifys, a close, an invoke, a remove and a clear each signalled
    // (the transient Notify stores nothing, so it does not).
    let mut ticks = 0;
    while changes.recv_timeout(Duration::from_millis(500)).is_ok() {
        ticks += 1;
    }
    assert!(ticks >= 6, "expected Changed signals, got {ticks}");
}

#[test]
#[ignore = "needs dbus-daemon; spawns a private bus"]
fn the_name_is_never_stolen_from_a_running_daemon() {
    let (_bus, address) = private_bus();
    // SAFETY: single-threaded, before any bus connection is made.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
    isolate_state();

    let _first = notifications::spawn().expect("the first one takes the name");
    assert!(
        notifications::spawn().is_err(),
        "a second server must fail rather than take the name from the first"
    );
}

/// Reads the real system bus. Safe to run anywhere: `status` is read-only by
/// construction, so the worst case on a machine without NetworkManager, BlueZ
/// or UPower is that nothing arrives and the assertions below are vacuous.
///
/// Ignored because it depends on which daemons the host happens to run.
#[test]
#[ignore = "reads the host's system bus"]
fn the_system_bus_answers_what_the_bar_asks_it() {
    use ec_services::status;

    let status = status::spawn().expect("system bus");

    // Each watcher reads once before it waits for a signal, so the first
    // readings land promptly. A second is generous for three round trips.
    std::thread::sleep(std::time::Duration::from_secs(1));

    let mut seen = Vec::new();
    while let Some(update) = status.try_recv() {
        seen.push(update);
    }
    eprintln!("{seen:#?}");
    assert!(!seen.is_empty(), "no daemon on the system bus answered");
}

/// Asks logind what it would allow. Deliberately only asks — nothing in this
/// test calls `perform`, because a test suite that can suspend the machine it
/// is running on is a test suite nobody runs twice.
#[test]
#[ignore = "reads the host's system bus"]
fn logind_says_what_it_would_allow() {
    use ec_services::session::{Action, Availability, Session};

    let session = Session::connect().expect("system bus");

    // Acting on our own session needs no permission from logind.
    assert_eq!(session.availability(Action::Lock), Availability::Yes);
    assert_eq!(session.availability(Action::LogOut), Availability::Yes);

    for action in [
        Action::Suspend,
        Action::Hibernate,
        Action::Reboot,
        Action::PowerOff,
    ] {
        eprintln!("{action:?}: {:?}", session.availability(action));
    }
}

/// The whole point of `org.freedesktop.ScreenSaver`: a client asks the bus to
/// keep the screen on, the level goes up, and when the client leaves the bus
/// without asking to release, it goes back down.
#[test]
#[ignore = "needs dbus-daemon; spawns a private bus"]
fn an_inhibit_holds_until_the_client_leaves_the_bus() {
    use ec_services::screensaver;
    use std::sync::mpsc;

    let (_bus, address) = private_bus();
    // SAFETY: single-threaded, before any bus connection is made.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };

    let (tx, levels) = mpsc::channel();
    let _service = screensaver::spawn_with(tx).expect("owns the name");

    let client = zbus::blocking::Connection::session().expect("client connection");
    macro_rules! call {
        ($method:expr, $body:expr) => {
            client.call_method(
                Some("org.freedesktop.ScreenSaver"),
                "/org/freedesktop/ScreenSaver",
                Some("org.freedesktop.ScreenSaver"),
                $method,
                $body,
            )
        };
    }

    let cookie: u32 = call!("Inhibit", &("firefox", "video"))
        .expect("Inhibit")
        .body()
        .deserialize()
        .expect("a cookie");
    assert_ne!(cookie, 0);
    assert_eq!(levels.recv_timeout(Duration::from_secs(5)), Ok(true));

    // Explicit release, then a second inhibit that is never released.
    call!("UnInhibit", &(cookie,)).expect("UnInhibit");
    assert_eq!(levels.recv_timeout(Duration::from_secs(5)), Ok(false));
    call!("Inhibit", &("firefox", "video")).expect("Inhibit again");
    assert_eq!(levels.recv_timeout(Duration::from_secs(5)), Ok(true));

    // Closing the connection is what a crashed or closed browser looks like.
    drop(client);
    assert_eq!(
        levels.recv_timeout(Duration::from_secs(5)),
        Ok(false),
        "a vanished client must not pin the screen on"
    );
}
